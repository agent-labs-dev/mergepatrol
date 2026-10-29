// PROTOTYPE: variants of the queue's Activity list, switched by `?variant=` on /$account/queue.
// Question: how should a PR's activity read, so you see what happened and what it cost at a glance?
// A is today's list. Throw the losers away once one wins.
import { type ReactNode, useState } from "react";
import type { EventView } from "../api/EventView";
import { timeAgo } from "../ui";

export type ActivityProps = {
  /** Newest first, as the API returns them. */
  events: EventView[];
  repo: string;
  trunk: string;
  /** Renders a message with its `#n` PR links. */
  linkify: (message: string) => ReactNode;
};

export const VARIANTS = [
  ["A", "Current list"],
  ["B", "Runs and steps"],
  ["C", "Timeline rail"],
  ["D", "Summary + log"],
] as const;

/* ---------- parsing shared by the variants ---------- */

const SHA = /^[0-9a-f]{40}$/;
const isSha = (s: string) => SHA.test(s);
const short = (sha: string) => sha.slice(0, 7);

/** `5 turns, $0.03`, maybe after a prefix: `<files> in 46 turns, $0.46`. */
function spend(message: string) {
  const m = message.match(/(?:^|\bin )(\d+) turns?, \$([\d.]+)$/);
  if (!m) return null;
  return { turns: Number(m[1]), cost: Number(m[2]), rest: message.slice(0, m.index).trim() };
}

const files = (s: string) => s.split(", ").filter(Boolean);
const fileName = (path: string) => path.split("/").at(-1) ?? path;

/** Queue-level kinds that start a step. `fixing` only with an attempt: the job logs its own. */
const startsStep = (e: EventView) =>
  ["enqueued", "rebasing", "held", "merged", "ejected", "dequeued"].includes(e.kind) ||
  (e.kind === "fixing" && /attempt \d+ of \d+/.test(e.message));

type Step = { start: EventView; events: EventView[] };
type Run = { steps: Step[]; at: string };

/** Oldest first: a run per rebase onto the trunk, a step per queue-level event. */
function toRuns(newestFirst: EventView[]): Run[] {
  const runs: Run[] = [];
  for (const e of [...newestFirst].reverse()) {
    if (e.kind === "rebasing" || runs.length === 0) {
      // The enqueue joins the first run rather than standing alone.
      const prev = runs.at(-1);
      if (prev && prev.steps.length === 1 && prev.steps[0].start.kind === "enqueued") {
        prev.steps.push({ start: e, events: [] });
        continue;
      }
      runs.push({ steps: [{ start: e, events: [] }], at: e.at });
      continue;
    }
    const run = runs[runs.length - 1];
    if (startsStep(e)) run.steps.push({ start: e, events: [] });
    else run.steps[run.steps.length - 1].events.push(e);
  }
  return runs;
}

type Status = "done" | "running" | "failed" | "interrupted";

function summarize(step: Step, live: boolean) {
  const find = (k: string) => step.events.find((e) => e.kind === k);
  const all = (k: string) => step.events.filter((e) => e.kind === k);
  const published = all("published").at(-1)?.message;
  const job = find("job")?.message;
  const s = step.start;
  const ai = [find("resolved"), find("fix")].flatMap((e) => (e ? [spend(e.message)] : []))[0];
  let status: Status = published ? "done" : live ? "running" : "interrupted";
  let title = s.kind;
  let detail: string | null = null;
  if (s.kind === "enqueued") {
    title = "Added to the queue";
    status = "done";
  } else if (s.kind === "rebasing") {
    title = `Rebase ${s.message}`;
    const conflict = find("conflict");
    if (conflict) detail = `${files(conflict.message).length} conflicts, resolved by Claude`;
    else if (published) detail = "clean";
  } else if (s.kind === "fixing") {
    const [what, attempt] = s.message.split(", ");
    title = `Fix ${what}`;
    detail = attempt ?? null;
  } else if (s.kind === "merged") {
    title = "Merged";
    status = "done";
  } else if (s.kind === "ejected") {
    title = "Ejected";
    detail = s.message;
    status = "failed";
  } else if (s.kind === "held") {
    title = "Held";
    detail = s.message;
  }
  if (step.events.some((e) => e.kind.endsWith("failed"))) status = "failed";
  return { title, detail, status, published, job, ai, conflict: find("conflict") };
}

/* ---------- small pieces ---------- */

function Sha({ repo, sha }: { repo: string; sha: string }) {
  return (
    <a
      href={`https://github.com/${repo}/commit/${sha}`}
      target="_blank"
      rel="noreferrer"
      title={sha}
      className="rounded bg-sunken px-1 py-px font-mono text-xs text-fg-2 hover:text-fg hover:underline"
    >
      {short(sha)}
    </a>
  );
}

function Cost({ turns, cost }: { turns: number; cost: number }) {
  return (
    <span className="inline-flex items-center gap-1 rounded bg-violet-50 px-1.5 py-px text-xs text-violet-700 tabular-nums dark:bg-violet-400/10 dark:text-violet-300">
      <span aria-hidden>✦</span>
      {turns} turns · ${cost.toFixed(2)}
    </span>
  );
}

function Time({ at }: { at: string }) {
  return (
    <time
      className="shrink-0 text-xs text-faint tabular-nums"
      title={new Date(at).toLocaleString()}
    >
      {timeAgo(at)}
    </time>
  );
}

const statusIcon: Record<Status, [string, string, string]> = {
  done: ["✓", "text-ok", "Done"],
  running: ["●", "text-warn animate-pulse", "Running"],
  failed: ["✕", "text-bad", "Failed"],
  interrupted: ["↺", "text-faint", "Superseded by a later run"],
};

function StatusIcon({ status }: { status: Status }) {
  const [icon, color, label] = statusIcon[status];
  return (
    <span className={`w-4 text-center ${color}`} title={label}>
      {icon}
    </span>
  );
}

/* ---------- A: today's list ---------- */

function VariantA({ events, linkify }: ActivityProps) {
  return (
    <ol className="overflow-hidden rounded-lg border border-line bg-surface empty:hidden">
      {events.map((ev) => (
        <li
          key={`${ev.at} ${ev.kind}`}
          className="flex items-baseline gap-3 border-line-soft px-4 py-2.5 not-first:border-t"
        >
          <span className="w-20 shrink-0 font-medium capitalize">{ev.kind}</span>
          <span className="min-w-0 flex-1 font-mono text-xs break-words whitespace-pre-wrap text-muted">
            {linkify(ev.message)}
          </span>
          <Time at={ev.at} />
        </li>
      ))}
    </ol>
  );
}

/* ---------- B: runs, each a card of steps; raw events behind a disclosure ---------- */

function VariantB({ events, repo, linkify }: ActivityProps) {
  const runs = toRuns(events);
  const [open, setOpen] = useState<Set<string>>(new Set());
  const toggle = (k: string) =>
    setOpen((s) => {
      const n = new Set(s);
      n.has(k) ? n.delete(k) : n.add(k);
      return n;
    });
  return (
    <div className="space-y-3">
      {runs
        .map((run, i) => ({ run, i, last: i === runs.length - 1 }))
        .reverse()
        .map(({ run, i, last }) => {
          const steps = run.steps.map((s) => ({ s, sum: summarize(s, last) }));
          const head = [...steps].reverse().find((x) => x.sum.published)?.sum.published;
          const cost = steps.reduce((t, x) => t + (x.sum.ai?.cost ?? 0), 0);
          return (
            <section
              key={run.at}
              className={`overflow-hidden rounded-lg border border-line bg-surface ${last ? "" : "opacity-75"}`}
            >
              <header className="flex items-center gap-2 border-b border-line-soft bg-sunken/50 px-4 py-2 text-xs">
                <span className="font-medium text-fg">Run {i + 1}</span>
                <span className="text-muted">
                  {last ? "current" : "superseded: the head or trunk moved"}
                </span>
                <span className="ml-auto flex items-center gap-2">
                  {cost > 0 && (
                    <span className="text-violet-600 dark:text-violet-300 tabular-nums">
                      ${cost.toFixed(2)}
                    </span>
                  )}
                  {head && <Sha repo={repo} sha={head} />}
                  <Time at={run.at} />
                </span>
              </header>
              <ol>
                {steps.map(({ s, sum }) => {
                  const key = `${s.start.at} ${s.start.kind}`;
                  const expandable = s.events.length > 0;
                  return (
                    <li key={key} className="border-line-soft not-first:border-t">
                      <button
                        type="button"
                        disabled={!expandable}
                        onClick={() => toggle(key)}
                        className="flex w-full items-center gap-3 px-4 py-2.5 text-left enabled:hover:bg-page"
                      >
                        <StatusIcon status={sum.status} />
                        <span className="min-w-0 flex-1">
                          <span className="font-medium">{sum.title}</span>
                          {sum.detail && <span className="ml-2 text-muted">{sum.detail}</span>}
                        </span>
                        {sum.ai && <Cost {...sum.ai} />}
                        {sum.published && (
                          <span className="text-xs text-faint">
                            → <Sha repo={repo} sha={sum.published} />
                          </span>
                        )}
                        <span className="w-3 text-xs text-faint">
                          {expandable ? (open.has(key) ? "▾" : "▸") : ""}
                        </span>
                      </button>
                      {open.has(key) && (
                        <ol className="space-y-1 bg-page px-4 py-2 pl-11 font-mono text-xs text-muted">
                          {s.events.map((e) => (
                            <li key={`${e.at} ${e.kind}`} className="flex gap-3">
                              <span className="w-16 shrink-0 text-fg-2">{e.kind}</span>
                              <span className="min-w-0 flex-1 break-words">
                                {linkify(e.message)}
                              </span>
                              <Time at={e.at} />
                            </li>
                          ))}
                        </ol>
                      )}
                    </li>
                  );
                })}
              </ol>
            </section>
          );
        })}
    </div>
  );
}

/* ---------- C: vertical rail, one humane sentence per event, jobs folded in ---------- */

const tone = {
  neutral: "bg-neutral-400",
  rebase: "bg-[var(--step-1)]",
  ai: "bg-violet-500",
  warn: "bg-amber-500",
  ok: "bg-emerald-500",
  bad: "bg-red-500",
};

function humanize(
  e: EventView,
  repo: string,
  trunk: string,
): { tone: keyof typeof tone; body: ReactNode } {
  const sha = isSha(e.message) ? <Sha repo={repo} sha={e.message} /> : null;
  switch (e.kind) {
    case "enqueued":
      return { tone: "neutral", body: "Added to the queue" };
    case "rebasing":
      return {
        tone: "rebase",
        body: (
          <>
            Rebasing onto <b className="font-medium">{trunk}</b>
          </>
        ),
      };
    case "conflict":
      return {
        tone: "warn",
        body: (
          <>
            Conflicts in {files(e.message).length} files
            <FileChips paths={files(e.message)} />
          </>
        ),
      };
    case "resolved": {
      const s = spend(e.message);
      return { tone: "ai", body: <>Claude resolved the conflicts {s && <Cost {...s} />}</> };
    }
    case "rebased":
      return { tone: "rebase", body: <>Rebased as {sha}</> };
    case "published":
      return { tone: "ok", body: <>Pushed {sha} to the PR</> };
    case "fixing": {
      const [what, attempt] = e.message.split(", ");
      return attempt
        ? {
            tone: "bad",
            body: (
              <>
                <code className="text-xs">{what}</code> failed, fixing ({attempt})
              </>
            ),
          }
        : {
            tone: "ai",
            body: (
              <>
                Claude is fixing <code className="text-xs">{what}</code>
              </>
            ),
          };
    }
    case "fix": {
      const s = spend(e.message);
      return { tone: "ai", body: <>Claude finished the fix {s && <Cost {...s} />}</> };
    }
    case "fixed":
      return { tone: "ok", body: <>Committed the fix as {sha}</> };
    case "merged":
      return { tone: "ok", body: <>Merged {sha}</> };
    case "ejected":
      return { tone: "bad", body: <>Ejected: {e.message}</> };
    case "held":
      return { tone: "warn", body: <>Held: {e.message}</> };
    default:
      return {
        tone: "neutral",
        body: (
          <>
            <span className="capitalize">{e.kind}</span> {sha ?? e.message}
          </>
        ),
      };
  }
}

function FileChips({ paths }: { paths: string[] }) {
  return (
    <span className="mt-1 flex flex-wrap gap-1">
      {paths.map((p) => (
        <span
          key={p}
          title={p}
          className="rounded border border-line px-1.5 font-mono text-[11px] text-fg-2"
        >
          {fileName(p)}
        </span>
      ))}
    </span>
  );
}

function VariantC({ events, repo, trunk }: ActivityProps) {
  // Job IDs are plumbing: fold each into the event before it as a hover.
  const rows: { e: EventView; job?: string }[] = [];
  for (const e of [...events].reverse()) {
    if (e.kind === "job" && rows.length) rows[rows.length - 1].job = e.message;
    else rows.push({ e });
  }
  rows.reverse();
  return (
    <ol className="relative ml-2 border-l border-line pl-5">
      {rows.map(({ e, job }, i) => {
        const h = humanize(e, repo, trunk);
        const restart = e.kind === "rebasing" && i < rows.length - 2;
        return (
          <li
            key={`${e.at} ${e.kind}`}
            className={`relative pb-4 ${restart ? "mb-4 border-b border-dashed border-line pb-6" : ""}`}
          >
            <span
              className={`absolute top-1.5 -left-[25px] size-2.5 rounded-full ring-4 ring-page ${tone[h.tone]}`}
            />
            <div className="flex items-baseline gap-3">
              <div className="min-w-0 flex-1 leading-relaxed">{h.body}</div>
              <Time at={e.at} />
            </div>
            {job && (
              <p className="mt-0.5 font-mono text-[11px] text-faint" title="Sandbox job">
                job {job.split("-").at(-1)}
              </p>
            )}
            {restart && (
              <p className="absolute -bottom-2.5 left-0 bg-page pr-2 text-xs text-faint">
                restarted
              </p>
            )}
          </li>
        );
      })}
    </ol>
  );
}

/* ---------- D: summary strip + pipeline, then a filterable dense log ---------- */

const pipeline = ["Queued", "Rebase", "Checks", "Fix", "Merge"] as const;

function VariantD({ events, repo, linkify }: ActivityProps) {
  const [filter, setFilter] = useState<"key" | "all">("key");
  const runs = toRuns(events);
  const oldest = events.at(-1);
  const ai = events
    .flatMap((e) => (["resolved", "fix"].includes(e.kind) ? [spend(e.message)] : []))
    .filter((x) => x !== null);
  const turns = ai.reduce((t, x) => t + x.turns, 0);
  const cost = ai.reduce((t, x) => t + x.cost, 0);
  const head = events.find((e) => e.kind === "published")?.message;
  const latest = runs.at(-1);
  const kinds = new Set(
    latest?.steps.flatMap((s) => [s.start.kind, ...s.events.map((e) => e.kind)]),
  );
  const reached: Record<(typeof pipeline)[number], Status | "todo"> = {
    Queued: "done",
    Rebase: kinds.has("published") ? "done" : kinds.has("rebasing") ? "running" : "todo",
    Checks: kinds.has("fixing") ? "failed" : kinds.has("published") ? "running" : "todo",
    Fix: kinds.has("fixed") ? "done" : kinds.has("fixing") ? "running" : "todo",
    Merge: kinds.has("merged") ? "done" : "todo",
  };
  if (reached.Fix === "done") reached.Checks = "running";
  const noisy = new Set(["job", "rebasing", "fixing"]);
  const shown =
    filter === "all"
      ? events
      : events.filter((e) => !noisy.has(e.kind) || /attempt/.test(e.message));
  return (
    <div className="space-y-4">
      <dl className="grid grid-cols-4 divide-x divide-line overflow-hidden rounded-lg border border-line bg-surface">
        {[
          ["In queue", oldest ? timeAgo(oldest.at).replace(" ago", "") : "—"],
          ["Runs", String(runs.length)],
          ["Claude", cost ? `$${cost.toFixed(2)}` : "—", turns ? `${turns} turns` : ""],
          ["Head", head ? <Sha key="h" repo={repo} sha={head} /> : "—"],
        ].map(([label, value, sub]) => (
          <div key={String(label)} className="px-4 py-3">
            <dt className="text-xs text-muted">{label}</dt>
            <dd className="mt-0.5 text-base font-semibold tabular-nums">
              {value} {sub && <span className="text-xs font-normal text-faint">{sub}</span>}
            </dd>
          </div>
        ))}
      </dl>
      <ol className="flex items-center gap-1 text-xs">
        {pipeline.map((p, i) => {
          const st = reached[p];
          const style =
            st === "done"
              ? "bg-emerald-50 text-emerald-700 ring-emerald-600/20 dark:bg-emerald-400/10 dark:text-emerald-300"
              : st === "running"
                ? "bg-amber-50 text-amber-800 ring-amber-600/20 dark:bg-amber-400/10 dark:text-amber-300"
                : st === "failed"
                  ? "bg-red-50 text-red-700 ring-red-600/20 dark:bg-red-400/10 dark:text-red-300"
                  : "bg-sunken text-faint ring-transparent";
          return (
            <li key={p} className="flex flex-1 items-center gap-1">
              <span
                className={`flex-1 rounded-md px-2 py-1.5 text-center font-medium ring-1 ring-inset ${style}`}
              >
                {st !== "todo" && <span className="mr-1">{statusIcon[st][0]}</span>}
                {p}
              </span>
              {i < pipeline.length - 1 && <span className="text-faint">›</span>}
            </li>
          );
        })}
      </ol>
      <div>
        <div className="mb-2 flex items-center gap-1 text-xs">
          {(["key", "all"] as const).map((f) => (
            <button
              key={f}
              type="button"
              onClick={() => setFilter(f)}
              className={`rounded px-2 py-0.5 ${filter === f ? "bg-fg text-surface" : "text-muted hover:bg-sunken"}`}
            >
              {f === "key" ? "Key events" : `All ${events.length}`}
            </button>
          ))}
        </div>
        <table className="w-full overflow-hidden rounded-lg text-xs">
          <tbody>
            {shown.map((e) => {
              const s = spend(e.message);
              return (
                <tr key={`${e.at} ${e.kind}`} className="border-b border-line-soft align-baseline">
                  <td className="w-16 py-1.5 pr-3 text-faint tabular-nums whitespace-nowrap">
                    {timeAgo(e.at)}
                  </td>
                  <td className="w-20 py-1.5 pr-3 font-medium capitalize">{e.kind}</td>
                  <td className="py-1.5 text-muted">
                    {isSha(e.message) ? (
                      <Sha repo={repo} sha={e.message} />
                    ) : e.kind === "conflict" ? (
                      <FileChips paths={files(e.message)} />
                    ) : s ? (
                      <span className="flex flex-wrap items-center gap-2">
                        {s.rest && <FileChips paths={files(s.rest)} />}
                        <Cost {...s} />
                      </span>
                    ) : (
                      <span className="font-mono">{linkify(e.message)}</span>
                    )}
                  </td>
                </tr>
              );
            })}
          </tbody>
        </table>
      </div>
    </div>
  );
}

export function Activity({ variant, ...props }: ActivityProps & { variant: string }) {
  if (variant === "B") return <VariantB {...props} />;
  if (variant === "C") return <VariantC {...props} />;
  if (variant === "D") return <VariantD {...props} />;
  return <VariantA {...props} />;
}
