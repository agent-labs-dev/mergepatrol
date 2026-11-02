import { useMutation, useQueries, useQuery, useQueryClient } from "@tanstack/react-query";
import { useState } from "react";
import { kindNames } from "./AgentReview";
import { capture } from "./analytics";
import {
  fetchRepoConfig,
  fetchRepos,
  fetchReviewSettings,
  proposeRepoConfig,
  setReviewSettings,
} from "./api";
import type { BandView } from "./api/BandView";
import type { BarView } from "./api/BarView";
import type { KindView } from "./api/KindView";
import type { PullRequestRef } from "./api/PullRequestRef";
import type { RepoConfigView } from "./api/RepoConfigView";
import type { RepoReviewView } from "./api/RepoReviewView";
import type { ReviewSettingsView } from "./api/ReviewSettingsView";
import { ErrorText, primary, Section, secondary, Toggle } from "./ui";

/*
 * How sure the agent must be to send each kind of change back to the PR's author: the workspace's bars, set
 * against how the agent's confidence has held up, and each repo's own, from its config file.
 */

type Bar = BarView["bar"];

const KINDS: KindView[] = ["security", "migrations", "dependencies", "public_api", "other"];

const kindHelp: Record<KindView, string> = {
  security: "auth, secrets, crypto, input that crosses a trust boundary",
  migrations: "schema and data migrations",
  dependencies: "lockfiles, new or bumped packages",
  public_api: "routes, exported types, CLI flags, config keys",
  other: "",
};

const barText = (b: Bar) => (b === "always_ask" ? "Always ask" : `${b}%`);
const barOf = (bars: BarView[], kind: KindView): Bar | undefined =>
  bars.find((b) => b.kind === kind)?.bar;

export function ReviewSettings({ workspace }: { workspace: number }) {
  const settings = useQuery({
    queryKey: ["review-settings", workspace],
    queryFn: () => fetchReviewSettings(workspace),
  });
  if (!settings.data) return null;
  return (
    <>
      <WorkspaceBars workspace={workspace} saved={settings.data} />
      <RepoBars
        workspace={workspace}
        workspaceBars={settings.data.bars}
        workspaceOn={settings.data.enabled}
      />
    </>
  );
}

function WorkspaceBars({ workspace, saved }: { workspace: number; saved: ReviewSettingsView }) {
  const client = useQueryClient();
  const [enabled, setEnabled] = useState(saved.enabled);
  const [resolved, setResolved] = useState(saved.require_resolved);
  const [bars, setBars] = useState<Record<KindView, Bar>>(
    () => Object.fromEntries(saved.bars.map((b) => [b.kind, b.bar])) as Record<KindView, Bar>,
  );
  const defaults = Object.fromEntries(saved.defaults.map((b) => [b.kind, b.bar])) as Record<
    KindView,
    Bar
  >;
  const save = useMutation({
    mutationFn: () =>
      setReviewSettings(workspace, {
        enabled,
        bars: KINDS.map((k) => ({ kind: k, bar: bars[k] })),
        require_resolved: resolved,
      }),
    onSuccess: (data) => {
      capture("review settings saved", { enabled: data.enabled });
      client.setQueryData(["review-settings", workspace], data);
    },
  });
  const dirty =
    enabled !== saved.enabled ||
    resolved !== saved.require_resolved ||
    KINDS.some((k) => bars[k] !== barOf(saved.bars, k));

  return (
    <Section
      id="review"
      title="Automatic review"
      sub="The agent reviews each queued PR. Findings it's sure of go back to the author as line comments; once every thread is resolved, the PR goes on without human approval."
      aside={<Toggle on={enabled} onChange={setEnabled} label="Automatic review" />}
    >
      <div className={`rounded-lg border border-line bg-surface ${enabled ? "" : "opacity-60"}`}>
        <div className="flex items-center justify-between gap-4 border-b border-line-soft p-4">
          <span>
            <span className="font-medium">Require resolved threads</span>
            <span className="block text-xs text-muted">
              Hold a queued PR while any review thread on it is open.
            </span>
          </span>
          <Toggle on={resolved} onChange={setResolved} label="Require resolved threads" />
        </div>
        <p className="px-4 pt-3 text-xs text-muted">
          <span className="font-medium text-fg-2">How sure the agent must be</span> to send a
          finding back, by kind. Lower sends more back. Charts: its findings of the last{" "}
          {saved.days} days by confidence; right of the line goes back, red was overruled.
        </p>
        <ul className="divide-y divide-line-soft">
          {KINDS.map((k) => (
            <KindRow
              key={k}
              kind={k}
              bar={bars[k]}
              fallback={defaults[k] === "always_ask" ? 90 : defaults[k]}
              bands={saved.history.find((h) => h.kind === k)?.bands ?? []}
              onChange={(bar) => setBars({ ...bars, [k]: bar })}
            />
          ))}
        </ul>
        <div className="flex items-center gap-3 border-t border-line-soft px-4 py-3">
          <span className="text-xs text-muted">
            {dirty ? "Unsaved. Applies to PRs reviewed after you save." : "Saved."}
          </span>
          <button
            type="button"
            onClick={() => setBars(defaults)}
            className="ml-auto text-sm text-muted hover:text-fg"
          >
            Defaults
          </button>
          <button
            type="button"
            disabled={!dirty || save.isPending}
            onClick={() => save.mutate()}
            className={`${primary} h-8 px-3`}
          >
            Save
          </button>
        </div>
        {save.error && (
          <div className="px-4 pb-3">
            <ErrorText error={save.error} />
          </div>
        )}
      </div>
    </Section>
  );
}

/** One kind's bar: its findings by confidence, the bar drawn on them, a slider, and what the bar
 * sends back. */
function KindRow({
  kind,
  bar,
  fallback,
  bands,
  onChange,
}: {
  kind: KindView;
  bar: Bar;
  /** Where the slider lands when Always ask goes off. */
  fallback: number;
  bands: BandView[];
  onChange: (bar: Bar) => void;
}) {
  const cut = bar === "always_ask" ? 101 : bar;
  const max = Math.max(1, ...bands.map((b) => b.findings));
  const total = bands.reduce((t, b) => t + b.findings, 0);
  const changes = bands.reduce((t, b) => t + b.changes, 0);
  const sentBack = bands.filter((b) => b.from >= cut).reduce((t, b) => t + b.changes, 0);
  const answered = bands.reduce((t, b) => t + b.answered, 0);
  const overruled = bands.reduce((t, b) => t + b.overruled, 0);
  return (
    <li className="grid items-center gap-x-5 gap-y-2 px-4 py-3 sm:grid-cols-[140px_120px_minmax(0,1fr)]">
      <span className="min-w-0">
        <span className="font-medium">{kindNames[kind]}</span>
        {kindHelp[kind] && (
          <span className="block truncate text-xs text-faint" title={kindHelp[kind]}>
            {kindHelp[kind]}
          </span>
        )}
      </span>
      <div className="relative flex h-8 items-end gap-0.5 max-sm:hidden">
        {bands.map((b) => (
          <div
            key={b.from}
            className={`flex flex-1 flex-col justify-end overflow-hidden rounded-t-sm ${
              b.from < cut
                ? "bg-neutral-200 dark:bg-neutral-400/30"
                : "bg-amber-200 dark:bg-amber-400/30"
            }`}
            style={{ height: `${b.findings ? Math.sqrt(b.findings / max) * 28 + 2 : 1}px` }}
            title={`${b.from}–${b.from + 5}% sure: ${b.findings} findings, ${b.overruled} overruled`}
          >
            <div
              className="w-full bg-red-500"
              style={{ height: `${b.findings ? (b.overruled / b.findings) * 100 : 0}%` }}
            />
          </div>
        ))}
        {bar !== "always_ask" && (
          <div
            className="pointer-events-none absolute inset-y-0 w-0.5 bg-fg"
            style={{ left: `${((bar - 50) / 50) * 100}%` }}
          />
        )}
      </div>
      <div className="min-w-0">
        <div className="flex items-center gap-3">
          <input
            type="range"
            aria-label={`${kindNames[kind]}: how sure the agent must be`}
            min={50}
            max={100}
            step={5}
            disabled={bar === "always_ask"}
            value={bar === "always_ask" ? 100 : bar}
            onChange={(e) => onChange(Number(e.target.value))}
            className="min-w-0 flex-1 accent-neutral-900 disabled:opacity-30 dark:accent-neutral-100"
          />
          <span
            className={`w-18 shrink-0 text-right text-sm tabular-nums ${
              bar === "always_ask" ? "text-warn" : ""
            }`}
          >
            {barText(bar)}
          </span>
          <label className="flex items-center gap-1.5 text-xs whitespace-nowrap text-muted">
            <input
              type="checkbox"
              checked={bar === "always_ask"}
              onChange={(e) => onChange(e.target.checked ? "always_ask" : fallback)}
            />
            Always ask
          </label>
        </div>
        <p className="mt-0.5 text-xs text-faint tabular-nums">
          {total === 0
            ? "No findings yet"
            : [
                bar === "always_ask" ? "A human sees each" : `${sentBack} of ${changes} sent back`,
                answered ? `${Math.round((overruled / answered) * 100)}% overruled` : null,
              ]
                .filter(Boolean)
                .join(" · ")}
        </p>
      </div>
    </li>
  );
}

const CHOICES: Bar[] = [55, 60, 65, 70, 75, 80, 85, 90, 95, "always_ask"];

/** Each queuing repo's own bars, from its config file. A change opens a PR there. */
function RepoBars({
  workspace,
  workspaceBars,
  workspaceOn,
}: {
  workspace: number;
  workspaceBars: BarView[];
  workspaceOn: boolean;
}) {
  const repos = useQuery({ queryKey: ["repos", workspace], queryFn: () => fetchRepos(workspace) });
  const on = (repos.data ?? []).filter((r) => r.on).map((r) => r.repo);
  const configs = useQueries({
    queries: on.map((repo) => ({
      queryKey: ["repo-config", workspace, repo],
      queryFn: () => fetchRepoConfig(workspace, repo),
    })),
  });
  const [staged, setStaged] = useState<Record<string, RepoReviewView>>({});
  const [opened, setOpened] = useState<Record<string, PullRequestRef>>({});
  const [reviewing, setReviewing] = useState(false);
  const [open, setOpen] = useState<string | null>(null);
  const client = useQueryClient();
  const loaded = on.flatMap((repo, i) => {
    const c = configs[i].data;
    return c ? [{ repo, config: c }] : [];
  });
  const propose = useMutation({
    mutationFn: async () => {
      const prs: Record<string, PullRequestRef> = {};
      for (const { repo, config } of loaded) {
        const review = staged[repo];
        if (!review) continue;
        prs[repo] = await proposeRepoConfig(workspace, repo, { ...config.settings, review });
      }
      return prs;
    },
    onSuccess: (prs) => {
      capture("repo review settings proposed", { repos: Object.keys(prs).length });
      setOpened({ ...opened, ...prs });
      setStaged({});
      setReviewing(false);
      for (const repo of Object.keys(prs))
        client.invalidateQueries({ queryKey: ["repo-config", workspace, repo] });
    },
  });

  const view = (repo: string, config: RepoConfigView) => staged[repo] ?? config.settings.review;
  const set = (repo: string, config: RepoConfigView, change: Partial<RepoReviewView>) =>
    setStaged({ ...staged, [repo]: { ...view(repo, config), ...change } });
  const setBar = (repo: string, config: RepoConfigView, kind: KindView, bar: Bar | undefined) => {
    const bars = view(repo, config).bars.filter((b) => b.kind !== kind);
    set(repo, config, { bars: bar === undefined ? bars : [...bars, { kind, bar }] });
  };
  const changed = Object.keys(staged);

  return (
    <Section
      id="repos"
      title="Repo overrides"
      sub="A repo follows the settings above unless its config file sets its own. Changing one opens a PR there."
    >
      <div className="rounded-lg border border-line bg-surface">
        {on.length === 0 && <p className="px-4 py-3 text-muted">No repo queues yet.</p>}
        <ul className="divide-y divide-line-soft">
          {loaded.map(({ repo, config }) => {
            const r = view(repo, config);
            const overrides = [
              ...(r.enabled === null ? [] : [`Review ${r.enabled ? "on" : "off"}`]),
              ...KINDS.flatMap((k) => {
                const own = barOf(r.bars, k);
                return own === undefined ? [] : [`${kindNames[k]} ${barText(own)}`];
              }),
            ];
            const expanded = open === repo;
            return (
              <li key={repo}>
                <div className="flex items-baseline gap-3 px-4 text-sm hover:bg-page ">
                  <button
                    type="button"
                    aria-expanded={expanded}
                    onClick={() => setOpen(expanded ? null : repo)}
                    className="flex min-w-0 flex-1 items-baseline gap-3 py-2.5 text-left"
                  >
                    <span className="shrink-0 font-medium" title={repo}>
                      {repo.split("/")[1]}
                    </span>
                    <span
                      className={`min-w-0 flex-1 truncate text-xs ${
                        overrides.length ? "text-fg-2" : "text-faint"
                      }`}
                    >
                      {overrides.length ? overrides.join(" · ") : "Follows the workspace"}
                    </span>
                    {staged[repo] && <span className="shrink-0 text-xs text-warn">edited</span>}
                    <span className="shrink-0 text-xs text-muted">
                      {expanded ? "Done" : "Edit"}
                    </span>
                  </button>
                  {opened[repo] && (
                    <a
                      href={opened[repo].url}
                      target="_blank"
                      rel="noreferrer"
                      className="shrink-0 text-xs text-ok"
                    >
                      PR #{opened[repo].number}
                    </a>
                  )}
                </div>
                {expanded && (
                  <div className="grid grid-cols-2 gap-x-4 gap-y-3 px-4 pt-1 pb-4 sm:grid-cols-3 ">
                    <RepoChoice
                      label="Review"
                      value={r.enabled === null ? "inherit" : r.enabled ? "on" : "off"}
                      inherit={workspaceOn ? "On" : "Off"}
                      changed={!!staged[repo] && r.enabled !== config.settings.review.enabled}
                      choices={[
                        ["on", "On"],
                        ["off", "Off"],
                      ]}
                      onChange={(v) =>
                        set(repo, config, { enabled: v === "inherit" ? null : v === "on" })
                      }
                    />
                    {KINDS.map((k) => {
                      const own = barOf(r.bars, k);
                      const ws = barOf(workspaceBars, k);
                      return (
                        <RepoChoice
                          key={k}
                          label={kindNames[k]}
                          value={own === undefined ? "inherit" : String(own)}
                          inherit={ws === undefined ? "" : barText(ws)}
                          changed={!!staged[repo] && own !== barOf(config.settings.review.bars, k)}
                          choices={CHOICES.map((c) => [String(c), barText(c)])}
                          onChange={(v) =>
                            setBar(
                              repo,
                              config,
                              k,
                              v === "inherit"
                                ? undefined
                                : v === "always_ask"
                                  ? "always_ask"
                                  : Number(v),
                            )
                          }
                        />
                      );
                    })}
                  </div>
                )}
              </li>
            );
          })}
        </ul>
        {changed.length > 0 && (
          <div className="flex items-center gap-3 border-t border-line px-4 py-3 ">
            <span className="text-xs text-muted">
              Changed {changed.map((r) => r.split("/")[1]).join(", ")}
            </span>
            <button
              type="button"
              onClick={() => setStaged({})}
              className="ml-auto text-sm text-muted hover:text-fg"
            >
              Discard
            </button>
            <button
              type="button"
              onClick={() => setReviewing(true)}
              className={`${primary} h-8 px-3`}
            >
              Review and open {changed.length === 1 ? "a PR" : `${changed.length} PRs`}
            </button>
          </div>
        )}
        {reviewing && (
          <div className="fixed inset-0 z-40 flex items-center justify-center bg-black/30 p-6">
            <div className="w-full max-w-xl rounded-lg bg-surface shadow-2xl ring-1 ring-line">
              <div className="border-b border-line px-5 py-3">
                <p className="font-medium">
                  Open {changed.length === 1 ? "a PR" : `${changed.length} PRs`} to change review
                  settings
                </p>
                <p className="text-sm text-muted">
                  It goes through review and the queue like any other change, and applies once it
                  merges.
                </p>
              </div>
              <div className="max-h-[50vh] space-y-4 overflow-y-auto px-5 py-4">
                {loaded
                  .filter(({ repo }) => staged[repo])
                  .map(({ repo, config }) => (
                    <div key={repo}>
                      <p className="mb-1 text-sm">
                        {repo} · <code className="text-xs">{config.path}</code>
                      </p>
                      <YamlDiff from={config.settings.review} to={staged[repo]} />
                    </div>
                  ))}
                <ErrorText error={propose.error} />
              </div>
              <div className="flex items-center justify-end gap-2 border-t border-line px-5 py-3">
                <button type="button" onClick={() => setReviewing(false)} className={secondary}>
                  Back
                </button>
                <button
                  type="button"
                  disabled={propose.isPending}
                  onClick={() => propose.mutate()}
                  className={primary}
                >
                  {propose.isPending ? "Opening…" : `Open ${changed.length === 1 ? "PR" : "PRs"}`}
                </button>
              </div>
            </div>
          </div>
        )}
      </div>
    </Section>
  );
}

/** One setting of a repo's: the workspace's value, shown with a dash, or its own. */
function RepoChoice({
  label,
  value,
  inherit,
  changed,
  choices,
  onChange,
}: {
  label: string;
  value: string;
  inherit: string;
  changed: boolean;
  choices: [string, string][];
  onChange: (value: string) => void;
}) {
  return (
    <label className="block text-xs text-muted">
      {label}
      <select
        value={value}
        onChange={(e) => onChange(e.target.value)}
        className={`mt-1 block h-8 w-full rounded-md border bg-transparent pl-2 text-sm ${
          changed
            ? "border-amber-500 text-fg"
            : value === "inherit"
              ? "border-line-soft text-faint"
              : "border-line text-fg-2"
        }`}
      >
        <option value="inherit">— {inherit}</option>
        {choices.map(([v, text]) => (
          <option key={v} value={v}>
            {text}
          </option>
        ))}
      </select>
    </label>
  );
}

/** The `review:` block as the config file has it. */
function yaml(r: RepoReviewView): string[] {
  const lines = ["review:"];
  if (r.enabled !== null) lines.push(`  enabled: ${r.enabled}`);
  const set = KINDS.filter((k) => barOf(r.bars, k) !== undefined);
  if (set.length) {
    lines.push("  bars:");
    for (const k of set) lines.push(`    ${k}: ${barOf(r.bars, k)}`);
  }
  return lines.length === 1 ? [] : lines;
}

function YamlDiff({ from, to }: { from: RepoReviewView; to: RepoReviewView }) {
  const a = yaml(from);
  const b = yaml(to);
  const lines = [
    ...a.filter((l) => !b.includes(l)).map((l) => ["-", l] as const),
    ...b.map((l) => [a.includes(l) ? " " : "+", l] as const),
  ];
  return (
    <pre className="overflow-x-auto rounded-md border border-line bg-page py-1 font-mono text-xs leading-5">
      {lines.length === 0 && (
        <div className="px-3 text-faint">No review settings: the workspace's apply.</div>
      )}
      {lines.map(([sign, l], i) => (
        <div
          // biome-ignore lint/suspicious/noArrayIndexKey: the lines of a diff never move.
          key={i}
          className={`px-3 ${
            sign === "+"
              ? "bg-emerald-50 dark:bg-emerald-400/10"
              : sign === "-"
                ? "bg-red-50 dark:bg-red-400/10"
                : ""
          }`}
        >
          <span className={sign === "+" ? "text-ok" : sign === "-" ? "text-bad" : "text-faint"}>
            {sign}
          </span>{" "}
          {l}
        </div>
      ))}
    </pre>
  );
}
