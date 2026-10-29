import { useQuery } from "@tanstack/react-query";
import { lazy, type ReactNode, Suspense, useState } from "react";
import { fetchFix } from "./api";
import type { EventView } from "./api/EventView";
import { duration, timeAgo } from "./ui";

/** A PR's activity: what its time in the queue has cost, how far its latest run got, and its
 * events. `events` are newest first, as the API returns them. */
export function Activity({
  workspace,
  events,
  repo,
  renderMessage,
}: {
  workspace: number;
  events: EventView[];
  repo: string;
  /** A message as text, with its `#n` PR links. */
  renderMessage: (message: string) => ReactNode;
}) {
  const [all, setAll] = useState(false);
  const shown = all ? events : events.filter(isKey);
  return (
    <div className="space-y-4">
      <Summary events={sinceEnqueued(events)} repo={repo} />
      <Stages run={latestRun(events)} />
      <div>
        <div className="mb-2 flex items-center gap-1 text-xs">
          <Filter on={!all} onClick={() => setAll(false)}>
            Key events
          </Filter>
          <Filter on={all} onClick={() => setAll(true)}>
            All {events.length}
          </Filter>
        </div>
        <table className="w-full text-xs">
          <tbody>
            {shown.map((e) => {
              const row = (message: ReactNode) => (
                // Events have no ID in the API yet.
                <tr key={`${e.at} ${e.kind}`} className="border-b border-line-soft align-baseline">
                  <td className="w-16 py-1.5 pr-3 whitespace-nowrap text-faint tabular-nums">
                    <time title={new Date(e.at).toLocaleString()}>{timeAgo(e.at)}</time>
                  </td>
                  <td className="w-20 py-1.5 pr-3 font-medium capitalize">{e.kind}</td>
                  <td className="py-1.5 text-muted">{message}</td>
                </tr>
              );
              return isFix(events, e) ? (
                <Fix
                  key={`${e.at} ${e.kind}`}
                  workspace={workspace}
                  repo={repo}
                  sha={e.message}
                  row={row}
                />
              ) : (
                row(<Message event={e} repo={repo} renderMessage={renderMessage} />)
              );
            })}
          </tbody>
        </table>
      </div>
    </div>
  );
}

/** The job's own progress, which the queue's events already tell: its ID, its verify commands,
 * the rebase starting, and its `fixing`, which repeats the queue's without the attempt. */
const isKey = (e: EventView) =>
  !["job", "verify", "rebasing"].includes(e.kind) && !(e.kind === "fixing" && !attempt(e));

const attempt = (e: EventView) => /attempt \d+ of \d+$/.test(e.message);

const isSha = (s: string) => /^[0-9a-f]{40}$/.test(s);

/** A `published` of the agent's fix: the queue publishes it right after the job's `fix`.
 * `events` are newest first. */
const isFix = (events: EventView[], e: EventView) =>
  e.kind === "published" && isSha(e.message) && events[events.indexOf(e) + 1]?.kind === "fix";

/** A `resolved` or `fix` event's agent run: `<files> in 46 turns, $0.46` or `5 turns`. */
function spend(e: EventView) {
  if (e.kind !== "resolved" && e.kind !== "fix") return null;
  const m = e.message.match(/^(?:(.*) in )?(\d+) turns?(?:, \$([\d.]+))?$/);
  if (!m) return null;
  return { files: m[1] ?? "", turns: Number(m[2]), cost: m[3] === undefined ? null : Number(m[3]) };
}

/** Newest first, back to the latest `kind`; every event without one. A copy: callers reverse
 * it, and `events` is the query's cache. */
function back(events: EventView[], kind: string) {
  const i = events.findIndex((e) => e.kind === kind);
  return events.slice(0, i === -1 ? undefined : i + 1);
}

/** Oldest first, from the last `enqueued`: a PR queued again starts over. */
const sinceEnqueued = (events: EventView[]) => back(events, "enqueued").reverse();

/** Oldest first, from the last rebase onto the trunk: the queue starts over when either side
 * moves. */
function latestRun(events: EventView[]) {
  const run = back(back(events, "enqueued"), "rebasing").reverse();
  return run[0]?.kind === "rebasing" ? run : [];
}

function Summary({ events, repo }: { events: EventView[]; repo: string }) {
  const spends = events.flatMap((e) => spend(e) ?? []);
  const turns = spends.reduce((t, s) => t + s.turns, 0);
  const cost = spends.reduce((t, s) => t + (s.cost ?? 0), 0);
  const head = events.filter((e) => e.kind === "published").at(-1)?.message;
  const since = events[0]?.at;
  return (
    // The gaps between tiles are the border showing through, in two rows or one.
    <dl className="grid grid-cols-2 gap-px overflow-hidden rounded-lg border border-line bg-line sm:grid-cols-4">
      <Tile label="In queue">
        {since ? duration((Date.now() - new Date(since).getTime()) / 1000) : "–"}
      </Tile>
      <Tile label="Runs">{events.filter((e) => e.kind === "rebasing").length}</Tile>
      <Tile label="Claude">
        {turns === 0 ? (
          "–"
        ) : (
          <>
            {cost > 0 && `$${cost.toFixed(2)} `}
            <span className={cost > 0 ? "text-xs font-normal text-faint" : ""}>{turns} turns</span>
          </>
        )}
      </Tile>
      <Tile label="Head">{head && isSha(head) ? <Sha repo={repo} sha={head} /> : "–"}</Tile>
    </dl>
  );
}

function Tile({ label, children }: { label: string; children: ReactNode }) {
  return (
    <div className="bg-surface px-4 py-3">
      <dt className="text-xs text-muted">{label}</dt>
      <dd className="mt-0.5 text-base font-semibold tabular-nums">{children}</dd>
    </div>
  );
}

const STAGES = ["Queued", "Rebase", "Checks", "Fix", "Merge"] as const;
type Stage = (typeof STAGES)[number];
type StageState = "done" | "running" | "held" | "failed" | "todo";

/** How far a run got, replaying its events in order. */
function stagesOf(run: EventView[]) {
  const s: Record<Stage, StageState> = {
    Queued: "done",
    Rebase: "todo",
    Checks: "todo",
    Fix: "todo",
    Merge: "todo",
  };
  for (const e of run) {
    if (e.kind === "rebasing") s.Rebase = "running";
    if (e.kind === "published") {
      if (s.Rebase === "running") s.Rebase = "done";
      if (s.Fix === "running") s.Fix = "done";
      s.Checks = "running";
    }
    if (e.kind === "fixing" && attempt(e)) {
      s.Checks = "failed";
      s.Fix = "running";
    }
    if (e.kind === "merged") {
      s.Checks = "done";
      s.Merge = "done";
    }
    // A hold can come at any point in the run, so it pauses the stage it lands on.
    if (e.kind === "held") for (const k of STAGES) if (s[k] === "running") s[k] = "held";
    if (e.kind === "ejected") for (const k of STAGES) if (s[k] === "running") s[k] = "failed";
  }
  return s;
}

const stageStyle: Record<StageState, [string, string]> = {
  done: [
    "✓",
    "bg-emerald-50 text-emerald-700 ring-emerald-600/20 dark:bg-emerald-400/10 dark:text-emerald-300 dark:ring-emerald-400/30",
  ],
  running: [
    "●",
    "bg-amber-50 text-amber-800 ring-amber-600/20 dark:bg-amber-400/10 dark:text-amber-300 dark:ring-amber-400/30",
  ],
  held: [
    "‖",
    "bg-amber-50 text-amber-800 ring-amber-600/20 dark:bg-amber-400/10 dark:text-amber-300 dark:ring-amber-400/30",
  ],
  failed: [
    "✕",
    "bg-red-50 text-red-700 ring-red-600/20 dark:bg-red-400/10 dark:text-red-300 dark:ring-red-400/30",
  ],
  todo: ["", "bg-sunken text-faint ring-transparent"],
};

function Stages({ run }: { run: EventView[] }) {
  const stages = stagesOf(run);
  return (
    <ol className="flex items-center gap-1 text-xs" aria-label="The latest run">
      {STAGES.map((stage, i) => {
        const [icon, style] = stageStyle[stages[stage]];
        return (
          <li key={stage} className="flex min-w-0 flex-1 items-center gap-1">
            <span
              className={`flex-1 truncate rounded-md px-1 py-1.5 text-center font-medium ring-1 ring-inset max-sm:text-[11px] sm:px-2 ${style}`}
            >
              {icon && <span className="mr-0.5 sm:mr-1">{icon}</span>}
              {stage}
            </span>
            {i < STAGES.length - 1 && <span className="text-faint max-sm:hidden">›</span>}
          </li>
        );
      })}
    </ol>
  );
}

function Message({
  event: e,
  repo,
  renderMessage,
}: {
  event: EventView;
  repo: string;
  renderMessage: (message: string) => ReactNode;
}) {
  if (isSha(e.message)) return <Sha repo={repo} sha={e.message} />;
  if (e.kind === "conflict") return <Files paths={e.message} />;
  const s = spend(e);
  if (s)
    return (
      <span className="flex flex-wrap items-center gap-1">
        {s.files && <Files paths={s.files} />}
        <span className="rounded bg-violet-50 px-1.5 py-px text-violet-700 tabular-nums dark:bg-violet-400/10 dark:text-violet-300">
          ✦ {s.turns} turns{s.cost !== null && ` · $${s.cost.toFixed(2)}`}
        </span>
      </span>
    );
  return (
    <span className="font-mono break-words whitespace-pre-wrap">{renderMessage(e.message)}</span>
  );
}

const FixDiff = lazy(() => import("./FixDiff"));

/** A published fix: its head, and what the fix changed, which opens below its row. */
function Fix({
  workspace,
  repo,
  sha,
  row,
}: {
  workspace: number;
  repo: string;
  sha: string;
  row: (message: ReactNode) => ReactNode;
}) {
  const [open, setOpen] = useState(false);
  const fix = useQuery({
    queryKey: ["fix", workspace, repo, sha],
    queryFn: () => fetchFix(workspace, repo, sha),
    // A commit never changes.
    staleTime: Infinity,
    retry: false,
  });
  const files = fix.data ? fileCount(fix.data.patch) : 0;
  return (
    <>
      {row(
        <span className="flex flex-wrap items-center gap-2">
          <Sha repo={repo} sha={sha} />
          {fix.data && (
            <button
              type="button"
              onClick={() => setOpen(!open)}
              aria-expanded={open}
              className="rounded border border-line px-1.5 py-px text-fg-2 hover:bg-sunken"
            >
              {open ? "▾" : "▸"} {files} {files === 1 ? "file" : "files"} ·{" "}
              <Changes patch={fix.data.patch} />
            </button>
          )}
        </span>,
      )}
      {open && fix.data && (
        <tr>
          <td colSpan={3} className="border-b border-line-soft py-2">
            <Suspense fallback={<p className="text-xs text-faint">Loading the diff…</p>}>
              <FixDiff fix={fix.data} />
            </Suspense>
          </td>
        </tr>
      )}
    </>
  );
}

const fileCount = (patch: string) => patch.match(/^diff --git /gm)?.length ?? 0;

/** A patch's added and removed lines: those in its hunks, not its file headers. */
function Changes({ patch }: { patch: string }) {
  let adds = 0;
  let dels = 0;
  let inHunk = false;
  for (const line of patch.split("\n")) {
    if (line.startsWith("diff --git ")) inHunk = false;
    else if (line.startsWith("@@")) inHunk = true;
    else if (inHunk && line.startsWith("+")) adds++;
    else if (inHunk && line.startsWith("-")) dels++;
  }
  return (
    <span className="font-mono tabular-nums">
      <span className="text-ok">+{adds}</span> <span className="text-bad">−{dels}</span>
    </span>
  );
}

function Sha({ repo, sha }: { repo: string; sha: string }) {
  return (
    <a
      href={`https://github.com/${repo}/commit/${sha}`}
      target="_blank"
      rel="noreferrer"
      title={sha}
      className="rounded bg-sunken px-1 py-px font-mono text-xs font-normal text-fg-2 hover:text-fg hover:underline"
    >
      {sha.slice(0, 7)}
    </a>
  );
}

/** A comma-separated list of paths, by file name. */
function Files({ paths }: { paths: string }) {
  return (
    <span className="flex flex-wrap gap-1">
      {paths.split(", ").map((p) => (
        <span
          key={p}
          title={p}
          className="rounded border border-line px-1.5 font-mono text-[11px] text-fg-2"
        >
          {p.split("/").at(-1)}
        </span>
      ))}
    </span>
  );
}

function Filter({
  on,
  onClick,
  children,
}: {
  on: boolean;
  onClick: () => void;
  children: ReactNode;
}) {
  return (
    <button
      type="button"
      onClick={onClick}
      aria-pressed={on}
      className={`rounded px-2 py-0.5 ${on ? "bg-fg text-surface" : "text-muted hover:bg-sunken"}`}
    >
      {children}
    </button>
  );
}
