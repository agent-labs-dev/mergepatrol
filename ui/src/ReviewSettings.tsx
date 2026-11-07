import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import { useState } from "react";
import { kindNames } from "./AgentReview";
import { capture } from "./analytics";
import { fetchReviewSettings, setReviewSettings } from "./api";
import type { BandView } from "./api/BandView";
import type { BarView } from "./api/BarView";
import type { KindView } from "./api/KindView";
import type { RepoReviewView } from "./api/RepoReviewView";
import type { ReviewSettingsView } from "./api/ReviewSettingsView";
import { ErrorText, primary, Section, Toggle } from "./ui";

/*
 * How sure the agent must be to send each kind of change back to the PR's author: the workspace's bars, set
 * against how the agent's confidence has held up, and the choices of a repo's own, for its config file.
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

type Depth = ReviewSettingsView["depth"];

const DEPTHS: [Depth, string, string][] = [
  ["quick", "Quick", "One reviewer, reporting only what could be wrong."],
  ["standard", "Standard", "One reviewer, which also notes what it checked and found sound."],
  [
    "deep",
    "Deep",
    "Three reviewers at once, and a panel of three votes on each change no failing test shows. Spends several times Standard: for code where a missed bug costs more.",
  ],
];
const depthName = (d: Depth) => DEPTHS.find(([v]) => v === d)?.[1] ?? d;

const barText = (b: Bar) => (b === "always_ask" ? "Always ask" : `${b}%`);
const barOf = (bars: BarView[], kind: KindView): Bar | undefined =>
  bars.find((b) => b.kind === kind)?.bar;

export function ReviewSettings({ workspace }: { workspace: number }) {
  const settings = useQuery({
    queryKey: ["review-settings", workspace],
    queryFn: () => fetchReviewSettings(workspace),
  });
  if (!settings.data) return null;
  return <WorkspaceBars workspace={workspace} saved={settings.data} />;
}

function WorkspaceBars({ workspace, saved }: { workspace: number; saved: ReviewSettingsView }) {
  const client = useQueryClient();
  const [enabled, setEnabled] = useState(saved.enabled);
  const [resolved, setResolved] = useState(saved.require_resolved);
  const [depth, setDepth] = useState(saved.depth);
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
        depth,
      }),
    onSuccess: (data) => {
      capture("review settings saved", { enabled: data.enabled, depth: data.depth });
      client.setQueryData(["review-settings", workspace], data);
    },
  });
  const dirty =
    enabled !== saved.enabled ||
    resolved !== saved.require_resolved ||
    depth !== saved.depth ||
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
        <div className="flex items-center justify-between gap-4 border-b border-line-soft p-4">
          <span>
            <span className="font-medium">Review depth</span>
            <span className="block text-xs text-muted">
              {DEPTHS.find(([v]) => v === depth)?.[2]}
            </span>
          </span>
          <select
            aria-label="Review depth"
            value={depth}
            onChange={(e) => setDepth(e.target.value as Depth)}
            className="h-8 shrink-0 rounded-md border border-line-strong bg-transparent pl-2 text-sm"
          >
            {DEPTHS.map(([v, name]) => (
              <option key={v} value={v}>
                {name}
              </option>
            ))}
          </select>
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

/** What a repo's review sets over the workspace, in words. */
export function reviewOverrides(r: RepoReviewView): string[] {
  return [
    ...(r.enabled === null ? [] : [`Review ${r.enabled ? "on" : "off"}`]),
    ...(r.require_resolved === undefined
      ? []
      : [`Resolved threads ${r.require_resolved ? "on" : "off"}`]),
    ...(r.depth === undefined ? [] : [`${depthName(r.depth)} review`]),
    ...KINDS.flatMap((k) => {
      const own = barOf(r.bars, k);
      return own === undefined ? [] : [`${kindNames[k]} ${barText(own)}`];
    }),
  ];
}

const ON_OFF: [string, string][] = [
  ["on", "On"],
  ["off", "Off"],
];
const onOff = (on: boolean) => (on ? "On" : "Off");

/** A repo's own review settings: each shows the workspace's until the repo sets one. */
export function RepoReview({
  workspace,
  review,
  saved,
  onChange,
}: {
  workspace: number;
  review: RepoReviewView;
  /** The file's, to mark what an edit changed. */
  saved: RepoReviewView;
  onChange: (review: RepoReviewView) => void;
}) {
  const settings = useQuery({
    queryKey: ["review-settings", workspace],
    queryFn: () => fetchReviewSettings(workspace),
  });
  const ws = settings.data;
  if (!ws) return null;
  const setBar = (kind: KindView, bar: Bar | undefined) => {
    const bars = review.bars.filter((b) => b.kind !== kind);
    onChange({ ...review, bars: bar === undefined ? bars : [...bars, { kind, bar }] });
  };
  const resolved = review.require_resolved;
  return (
    <div className="grid grid-cols-2 gap-x-4 gap-y-3 sm:grid-cols-3">
      <RepoChoice
        label="Review"
        value={review.enabled === null ? "inherit" : review.enabled ? "on" : "off"}
        inherit={onOff(ws.enabled)}
        changed={review.enabled !== saved.enabled}
        choices={ON_OFF}
        onChange={(v) => onChange({ ...review, enabled: v === "inherit" ? null : v === "on" })}
      />
      <RepoChoice
        label="Resolved threads"
        value={resolved === undefined ? "inherit" : resolved ? "on" : "off"}
        inherit={onOff(ws.require_resolved)}
        changed={resolved !== saved.require_resolved}
        choices={ON_OFF}
        onChange={(v) => {
          const { require_resolved: _, ...rest } = review;
          onChange(v === "inherit" ? rest : { ...rest, require_resolved: v === "on" });
        }}
      />
      <RepoChoice
        label="Depth"
        value={review.depth ?? "inherit"}
        inherit={depthName(ws.depth)}
        changed={review.depth !== saved.depth}
        choices={DEPTHS.map(([v, name]) => [v, name])}
        onChange={(v) => {
          const { depth: _, ...rest } = review;
          onChange(v === "inherit" ? rest : { ...rest, depth: v as Depth });
        }}
      />
      {KINDS.map((k) => {
        const own = barOf(review.bars, k);
        const theirs = barOf(ws.bars, k);
        return (
          <RepoChoice
            key={k}
            label={kindNames[k]}
            value={own === undefined ? "inherit" : String(own)}
            inherit={theirs === undefined ? "" : barText(theirs)}
            changed={own !== barOf(saved.bars, k)}
            choices={CHOICES.map((c) => [String(c), barText(c)])}
            onChange={(v) =>
              setBar(k, v === "inherit" ? undefined : v === "always_ask" ? v : Number(v))
            }
          />
        );
      })}
    </div>
  );
}

/** One setting of a repo's: the workspace's value, or its own. */
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
              : "border-line-strong font-medium text-fg"
        }`}
      >
        <option value="inherit">Workspace: {inherit}</option>
        {choices.map(([v, text]) => (
          <option key={v} value={v}>
            {text}
          </option>
        ))}
      </select>
    </label>
  );
}
