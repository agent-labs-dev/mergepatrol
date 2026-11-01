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
import type { BarView } from "./api/BarView";
import type { KindView } from "./api/KindView";
import type { PullRequestRef } from "./api/PullRequestRef";
import type { RepoConfigView } from "./api/RepoConfigView";
import type { RepoReviewView } from "./api/RepoReviewView";
import type { ReviewSettingsView } from "./api/ReviewSettingsView";
import { ErrorText, primary, secondary } from "./ui";

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
  const [kind, setKind] = useState<KindView>("other");
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

  const bar = bars[kind];
  const cut = bar === "always_ask" ? 101 : bar;
  const bands = saved.history.find((h) => h.kind === kind)?.bands ?? [];
  const total = bands.reduce((t, b) => t + b.findings, 0);
  const max = Math.max(1, ...bands.map((b) => b.findings));
  const changes = bands.reduce((t, b) => t + b.changes, 0);
  const sentBack = bands.filter((b) => b.from >= cut).reduce((t, b) => t + b.changes, 0);
  const answered = bands.reduce((t, b) => t + b.answered, 0);
  const overruled = bands.reduce((t, b) => t + b.overruled, 0);

  return (
    <section className="rounded-lg border border-line bg-surface">
      <div className="p-4 sm:p-5">
        <div className="flex items-start justify-between gap-4">
          <h2 className="font-semibold">Automatic review</h2>
          <label className="flex items-center gap-2 text-sm">
            <input
              type="checkbox"
              checked={enabled}
              onChange={(e) => setEnabled(e.target.checked)}
            />
            On
          </label>
        </div>
        <p className="mt-1 text-muted">
          The agent reviews every queued PR before it takes its place in line. A change it's sure
          enough of goes back to the PR's author, often the agent that built it, as a comment on its
          line. The author fixes it or says why not, and resolves the thread; once every thread is
          resolved, the PR goes on with no human approval. It asks a human only about a call the
          author can't make. It runs on the workspace's API key. Set how sure is sure enough to send
          a change back for each kind: lower sends back more. A repo can set its own below.
        </p>
        <label className="mt-3 flex items-start gap-2 text-sm">
          <input
            type="checkbox"
            className="mt-0.5"
            checked={resolved}
            onChange={(e) => setResolved(e.target.checked)}
          />
          <span>
            Require resolved comments
            <span className="block text-muted">
              Hold a queued PR while any review thread on it is unresolved, whoever started it.
            </span>
          </span>
        </label>
      </div>
      <div className="flex border-t border-line max-sm:flex-col">
        <ul className="shrink-0 border-line py-2 max-sm:border-b sm:w-44 sm:border-r">
          {KINDS.map((k) => (
            <li key={k}>
              <button
                type="button"
                onClick={() => setKind(k)}
                className={`block w-full px-4 py-1.5 text-left text-sm ${
                  k === kind
                    ? "bg-page font-medium shadow-[inset_2px_0_0_0_var(--color-fg)]"
                    : "text-muted hover:text-fg"
                }`}
              >
                {kindNames[k]}
                <span
                  className={`block text-xs font-normal tabular-nums ${
                    bars[k] === "always_ask" ? "text-warn" : "text-muted"
                  }`}
                >
                  {barText(bars[k])}
                  {bars[k] === defaults[k] && <span className="text-faint"> · default</span>}
                </span>
              </button>
            </li>
          ))}
        </ul>
        <div className="min-w-0 flex-1 p-4 sm:p-5">
          <p className="text-sm">
            <span className="font-medium">{kindNames[kind]}</span>
            {kindHelp[kind] && <span className="text-muted"> · {kindHelp[kind]}</span>}
          </p>
          <p className="mt-0.5 text-xs text-muted">
            The agent's findings of the last {saved.days} days, by how sure it was. A change from
            the bar up goes back to the author; under it, it's a note. Red: a human answering it
            decided otherwise than the agent leaned.
          </p>
          <div className="relative mt-8 flex h-32 items-end gap-1">
            {bands.map((b) => (
              <div
                key={b.from}
                className={`flex flex-1 flex-col justify-end overflow-hidden rounded-t ${
                  b.from < cut
                    ? "bg-neutral-200 dark:bg-neutral-400/30"
                    : "bg-amber-200 dark:bg-amber-400/30"
                }`}
                style={{ height: `${b.findings ? Math.sqrt(b.findings / max) * 120 + 4 : 2}px` }}
                title={`${b.from}–${b.from + 5}% sure: ${b.findings} findings, ${b.overruled} overruled`}
              >
                <div
                  className="w-full bg-red-500"
                  style={{ height: `${b.findings ? (b.overruled / b.findings) * 100 : 0}%` }}
                />
              </div>
            ))}
            {bar !== "always_ask" ? (
              <div
                className="pointer-events-none absolute inset-y-0 w-0.5 bg-fg"
                style={{ left: `${((bar - 50) / 50) * 100}%` }}
              >
                <span
                  className={`absolute -top-6 rounded bg-fg px-1.5 py-0.5 text-xs whitespace-nowrap text-surface ${
                    bar > 80 ? "right-1.5" : "left-1.5"
                  }`}
                >
                  sends back from {bar}%
                </span>
              </div>
            ) : (
              <span className="absolute -top-6 right-0 rounded bg-amber-500 px-1.5 py-0.5 text-xs text-white">
                always asks a human
              </span>
            )}
          </div>
          <div className="mt-1 flex justify-between text-xs text-faint tabular-nums">
            <span>50% sure</span>
            <span>75%</span>
            <span>100%</span>
          </div>
          <div className="mt-3 flex items-center gap-4">
            <input
              type="range"
              aria-label={`${kindNames[kind]}: how sure the agent must be`}
              min={50}
              max={100}
              step={5}
              disabled={bar === "always_ask"}
              value={bar === "always_ask" ? 100 : bar}
              onChange={(e) => setBars({ ...bars, [kind]: Number(e.target.value) })}
              className="flex-1 accent-neutral-900 disabled:opacity-30 dark:accent-neutral-100"
            />
            <label className="flex items-center gap-1.5 text-sm whitespace-nowrap">
              <input
                type="checkbox"
                checked={bar === "always_ask"}
                onChange={(e) =>
                  setBars({
                    ...bars,
                    [kind]: e.target.checked
                      ? "always_ask"
                      : defaults[kind] === "always_ask"
                        ? 90
                        : defaults[kind],
                  })
                }
              />
              Always ask
            </label>
          </div>
          {total === 0 ? (
            <p className="mt-4 rounded-md bg-sunken px-3 py-2 text-sm text-muted">
              No {kind === "other" ? "" : `${kindNames[kind].toLowerCase()} `}findings yet: the
              chart fills in as the agent reviews.
            </p>
          ) : (
            <div className="mt-4 grid grid-cols-3 gap-3 text-sm">
              <Stat
                label="Sent back"
                value={bar === "always_ask" ? "–" : String(sentBack)}
                sub={
                  bar === "always_ask"
                    ? "a human sees each"
                    : `of ${changes} changes the agent wanted`
                }
              />
              <Stat
                label="Overruled"
                value={answered ? `${Math.round((overruled / answered) * 100)}%` : "–"}
                sub={`of ${answered} answered`}
              />
              <Stat
                label="Notes"
                value={bar === "always_ask" ? "–" : String(changes - sentBack)}
                sub="under the bar, holding nothing"
              />
            </div>
          )}
        </div>
      </div>
      <div className="flex items-center gap-3 border-t border-line px-4 py-3 sm:px-5">
        <span className="text-xs text-muted">
          {dirty
            ? "Unsaved. Applies to PRs reviewed after you save."
            : "Saved. Every repo without its own bars uses these."}
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
        <div className="px-4 pb-3 sm:px-5">
          <ErrorText error={save.error} />
        </div>
      )}
    </section>
  );
}

function Stat({ label, value, sub }: { label: string; value: string; sub: string }) {
  return (
    <div className="rounded-md bg-sunken px-3 py-2">
      <p className="text-xs text-muted">{label}</p>
      <p className="text-xl font-semibold tabular-nums">{value}</p>
      <p className="text-xs text-faint">{sub}</p>
    </div>
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
  if (on.length === 0) return null;

  const view = (repo: string, config: RepoConfigView) => staged[repo] ?? config.settings.review;
  const set = (repo: string, config: RepoConfigView, change: Partial<RepoReviewView>) =>
    setStaged({ ...staged, [repo]: { ...view(repo, config), ...change } });
  const setBar = (repo: string, config: RepoConfigView, kind: KindView, bar: Bar | undefined) => {
    const bars = view(repo, config).bars.filter((b) => b.kind !== kind);
    set(repo, config, { bars: bar === undefined ? bars : [...bars, { kind, bar }] });
  };
  const changed = Object.keys(staged);

  return (
    <section className="rounded-lg border border-line bg-surface">
      <div className="p-4 sm:p-5">
        <h2 className="font-semibold">Each repo's review</h2>
        <p className="mt-1 text-muted">
          A dash follows the workspace. A repo's own bars live in its config file, so changing one
          opens a PR there.
        </p>
      </div>
      <div className="overflow-x-auto border-t border-line">
        <table className="w-full text-sm">
          <thead className="text-xs text-muted">
            <tr className="border-b border-line">
              <th className="px-4 py-2 text-left font-medium">Kind</th>
              <th className="px-3 py-2 text-left font-medium">Workspace</th>
              {loaded.map(({ repo }) => (
                <th key={repo} className="px-3 py-2 text-left font-medium whitespace-nowrap">
                  {repo.split("/")[1]}
                  {opened[repo] && (
                    <a
                      href={opened[repo].url}
                      target="_blank"
                      rel="noreferrer"
                      className="ml-1.5 font-normal text-ok"
                    >
                      PR #{opened[repo].number}
                    </a>
                  )}
                  {staged[repo] && <span className="ml-1.5 font-normal text-warn">edited</span>}
                </th>
              ))}
            </tr>
          </thead>
          <tbody className="divide-y divide-line-soft">
            <tr>
              <td className="px-4 py-2 font-medium">Review</td>
              <td className="px-3 py-2 text-muted">{workspaceOn ? "On" : "Off"}</td>
              {loaded.map(({ repo, config }) => {
                const enabled = view(repo, config).enabled;
                return (
                  <td key={repo} className="px-3 py-1.5">
                    <select
                      aria-label={`${repo}: review`}
                      value={enabled === null ? "inherit" : enabled ? "on" : "off"}
                      onChange={(e) =>
                        set(repo, config, {
                          enabled: e.target.value === "inherit" ? null : e.target.value === "on",
                        })
                      }
                      className={`h-7 rounded-md border bg-transparent pl-2 text-xs ${
                        enabled === null
                          ? "border-transparent text-faint hover:border-line"
                          : "border-line text-fg-2"
                      }`}
                    >
                      <option value="inherit">— {workspaceOn ? "On" : "Off"}</option>
                      <option value="on">On</option>
                      <option value="off">Off</option>
                    </select>
                  </td>
                );
              })}
            </tr>
            {KINDS.map((k) => {
              const ws = barOf(workspaceBars, k);
              return (
                <tr key={k}>
                  <td className="px-4 py-2 font-medium whitespace-nowrap">{kindNames[k]}</td>
                  <td className="px-3 py-2">
                    {ws !== undefined && (
                      <span className="rounded-md bg-sunken px-2 py-0.5 text-xs font-medium text-fg-2 ring-1 ring-line ring-inset">
                        {barText(ws)}
                      </span>
                    )}
                  </td>
                  {loaded.map(({ repo, config }) => {
                    const own = barOf(view(repo, config).bars, k);
                    const was = barOf(config.settings.review.bars, k);
                    return (
                      <td key={repo} className="px-3 py-1.5">
                        <select
                          aria-label={`${repo}: ${kindNames[k]}`}
                          value={own === undefined ? "inherit" : String(own)}
                          onChange={(e) =>
                            setBar(
                              repo,
                              config,
                              k,
                              e.target.value === "inherit"
                                ? undefined
                                : e.target.value === "always_ask"
                                  ? "always_ask"
                                  : Number(e.target.value),
                            )
                          }
                          className={`h-7 rounded-md border bg-transparent pl-2 text-xs ${
                            staged[repo] && own !== was
                              ? "border-amber-500 text-fg"
                              : own === undefined
                                ? "border-transparent text-faint hover:border-line"
                                : "border-line text-fg-2"
                          }`}
                        >
                          <option value="inherit">— {ws === undefined ? "" : barText(ws)}</option>
                          {CHOICES.map((c) => (
                            <option key={c} value={String(c)}>
                              {barText(c)}
                            </option>
                          ))}
                        </select>
                      </td>
                    );
                  })}
                </tr>
              );
            })}
          </tbody>
        </table>
      </div>
      {changed.length > 0 && (
        <div className="flex items-center gap-3 border-t border-line px-4 py-3 sm:px-5">
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
    </section>
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
