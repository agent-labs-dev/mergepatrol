import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import { type ReactNode, useEffect, useRef, useState } from "react";
import { capture } from "./analytics";
import { answerAgentReview, askAgent, askReviewers, fetchAgentReview } from "./api";
import type { AgentReviewView } from "./api/AgentReviewView";
import type { AnswerView } from "./api/AnswerView";
import type { CodeLineView } from "./api/CodeLineView";
import type { EntryView } from "./api/EntryView";
import type { FindingView } from "./api/FindingView";
import type { KindView } from "./api/KindView";
import { ErrorText, field, primary, secondary, timeAgo } from "./ui";

/*
 * The agent's review, the queue's first gate: where a PR is in it, in the queue's rows and
 * detail, and the screen where a human answers what the agent asks, one finding at a time.
 */

/** The kinds of finding as people say them. */
export const kindNames: Record<KindView, string> = {
  security: "Security",
  migrations: "Migrations",
  dependencies: "Dependencies",
  public_api: "Public API",
  other: "Everything else",
};

const kindTone: Record<KindView, string> = {
  security:
    "bg-red-50 text-red-700 ring-red-600/20 dark:bg-red-400/10 dark:text-red-300 dark:ring-red-400/30",
  migrations:
    "bg-amber-50 text-amber-800 ring-amber-600/20 dark:bg-amber-400/10 dark:text-amber-300 dark:ring-amber-400/30",
  dependencies:
    "bg-sky-50 text-sky-700 ring-sky-600/20 dark:bg-sky-400/10 dark:text-sky-300 dark:ring-sky-400/30",
  public_api:
    "bg-violet-50 text-violet-700 ring-violet-600/20 dark:bg-violet-400/10 dark:text-violet-300 dark:ring-violet-400/30",
  other: "bg-sunken text-fg-2 ring-neutral-500/20",
};

export const reviewTones = {
  reviewing:
    "bg-violet-50 text-violet-700 ring-violet-600/20 dark:bg-violet-400/10 dark:text-violet-300 dark:ring-violet-400/30",
  asks: "bg-amber-50 text-amber-800 ring-amber-600/20 dark:bg-amber-400/10 dark:text-amber-300 dark:ring-amber-400/30",
  sentBack:
    "bg-red-50 text-red-700 ring-red-600/20 dark:bg-red-400/10 dark:text-red-300 dark:ring-red-400/30",
};

type Gated = Pick<EntryView, "state" | "hold" | "review">;

/** The badge of a PR the review holds, in place of "Held". */
export function reviewBadge(e: Gated): [string, string] | null {
  if (e.state !== "held") return null;
  switch (e.hold?.reason) {
    case "reviewing":
      return ["Reviewing", reviewTones.reviewing];
    case "review_asks":
      return ["Needs a human", reviewTones.asks];
    case "review_changes":
      return ["Sent back", reviewTones.sentBack];
    case "unresolved_threads":
      return ["Unresolved comments", reviewTones.sentBack];
    default:
      return null;
  }
}

const people = (logins: string[]) => logins.map((l) => `@${l}`).join(", ");

/** The line under a queue row: where the PR is in the review. `null` without one to show. */
export function reviewLine(e: Gated): ReactNode {
  const r = e.review;
  if (!r) return null;
  if (e.state === "held") {
    switch (e.hold?.reason) {
      case "reviewing":
        return <span className="text-muted">The agent is reading the change…</span>;
      case "review_asks":
        return (
          <span className="text-warn">
            {r.failed
              ? "The agent could not review it: a human reviews it instead"
              : `${r.asks} ${r.asks === 1 ? "question" : "questions"} for ${
                  r.asked.length ? people(r.asked) : "a reviewer"
                } · ${r.cleared} cleared by the agent`}
          </span>
        );
      case "review_changes":
        return (
          <span className="text-muted">
            {r.answered_by
              ? `@${r.answered_by} asked for changes`
              : `${r.sent_back} sent back to the author`}{" "}
            · goes on once each comment is resolved
          </span>
        );
      case "unresolved_threads":
        return <span className="text-muted">{e.hold?.message}</span>;
      default:
        return null;
    }
  }
  if (r.status !== "approved") return null;
  return (
    <span className="text-muted">
      {r.answered_by
        ? `Approved by @${r.answered_by} on the agent's ${r.asks === 1 ? "question" : "questions"}`
        : `Agent approved alone · ${r.cleared} cleared`}
    </span>
  );
}

/** Text with `code` spans. */
export function Md({ children }: { children: string }) {
  return (
    <>
      {children.split(/`([^`]+)`/).map((part, i) =>
        i % 2 ? (
          // biome-ignore lint/suspicious/noArrayIndexKey: the parts of a text never move.
          <code key={i} className="rounded bg-sunken px-1 font-mono text-[0.85em]">
            {part}
          </code>
        ) : (
          part
        ),
      )}
    </>
  );
}

export function Pill({ children, tone }: { children: ReactNode; tone: string }) {
  return (
    <span
      className={`whitespace-nowrap rounded-md px-1.5 py-0.5 text-xs font-medium ring-1 ring-inset ${tone}`}
    >
      {children}
    </span>
  );
}

function KindPill({ kind }: { kind: KindView }) {
  return <Pill tone={kindTone[kind]}>{kindNames[kind]}</Pill>;
}

const pct = (n: number) => `${Math.round(n)}%`;

/** How sure the agent was, against the bar it was held to. */
function Confidence({ f }: { f: FindingView }) {
  return (
    <span className="inline-flex items-center gap-1.5 text-xs text-muted">
      <span className="relative h-1.5 w-16 rounded-full bg-sunken">
        <span
          className="absolute inset-y-0 left-0 rounded-full bg-amber-500"
          style={{ width: pct(f.confidence) }}
        />
        {f.bar !== "always_ask" && (
          <span className="absolute -inset-y-0.5 w-px bg-fg" style={{ left: pct(f.bar) }} />
        )}
      </span>
      <span className="tabular-nums">
        {pct(f.confidence)} sure
        <span className="text-faint">
          {f.lean === "change"
            ? " · leans to a change"
            : f.bar === "always_ask"
              ? " · always asks"
              : ` · decides alone from ${pct(f.bar)}`}
        </span>
      </span>
    </span>
  );
}

function Code({ lines, file }: { lines: CodeLineView[]; file: string }) {
  if (lines.length === 0) return null;
  return (
    <div className="overflow-hidden rounded-md border border-line bg-surface">
      <p className="border-b border-line bg-sunken px-3 py-1.5 font-mono text-xs text-muted">
        {file}
      </p>
      <pre className="overflow-x-auto py-1 font-mono text-xs leading-5">
        {lines.map((l, i) => (
          <div
            // biome-ignore lint/suspicious/noArrayIndexKey: the lines of an excerpt never move.
            key={i}
            className={`flex ${
              l.kind === "added"
                ? "bg-emerald-50 dark:bg-emerald-400/10"
                : l.kind === "removed"
                  ? "bg-red-50 dark:bg-red-400/10"
                  : ""
            } ${l.hot ? "shadow-[inset_3px_0_0_0_var(--color-amber-500)]" : ""}`}
          >
            <span className="w-10 shrink-0 pr-2 text-right text-faint select-none">{l.number}</span>
            <span
              className={`w-4 shrink-0 select-none ${
                l.kind === "added" ? "text-ok" : l.kind === "removed" ? "text-bad" : "text-faint"
              }`}
            >
              {l.kind === "added" ? "+" : l.kind === "removed" ? "-" : " "}
            </span>
            <span className="pr-4 whitespace-pre">{l.text}</span>
          </div>
        ))}
      </pre>
    </div>
  );
}

function Kbd({ children }: { children: ReactNode }) {
  return (
    <kbd className="rounded border border-line-strong bg-surface px-1 font-mono text-[10px] text-muted shadow-xs">
      {children}
    </kbd>
  );
}

/** Single keys, off while typing (but for Escape, as `esc`) or with a modifier. */
function useKeys(map: Record<string, () => unknown>) {
  useEffect(() => {
    const onKey = (e: KeyboardEvent) => {
      const key = e.key === "Escape" ? "esc" : e.key === "Enter" ? "enter" : e.key;
      const target = e.target as HTMLElement;
      if (target.closest("input, textarea, select") && key !== "esc") return;
      // A focused button or link takes its own Enter.
      if (key === "enter" && target.closest("button, a")) return;
      if (e.metaKey || e.ctrlKey || e.altKey) return;
      const f = map[key];
      if (f) {
        e.preventDefault();
        f();
      }
    };
    addEventListener("keydown", onKey);
    return () => removeEventListener("keydown", onKey);
  });
}

/** The PR's review, read again every few seconds while it moves: the agent at work, or a
 * question waiting for its answer. */
export function useAgentReview(workspace: number, repo: string, pr: number) {
  return useQuery({
    queryKey: ["agent-review", workspace, repo, pr],
    queryFn: () => fetchAgentReview(workspace, repo, pr),
    refetchInterval: (q) => {
      const r = q.state.data;
      const moving =
        r?.status === "pending" ||
        r?.status === "running" ||
        r?.questions.some((q) => !q.answer && !q.error);
      return moving ? 4000 : 30_000;
    },
  });
}

const STAGES = ["Agent review", "Human", "Wait", "Test", "Merge"] as const;

/** Where a PR is between the review and the merge. */
function Stages({ at, human }: { at: number; human: "skip" | "ask" }) {
  return (
    <ol className="flex flex-wrap items-center gap-1 text-xs">
      {STAGES.map((s, i) => {
        const skipped = s === "Human" && human === "skip";
        const now = i === at;
        return (
          <li key={s} className="flex items-center gap-1">
            <span
              className={`rounded-full px-2 py-0.5 ${
                skipped
                  ? "text-faint line-through"
                  : now
                    ? s === "Human"
                      ? "bg-amber-500 font-medium text-white"
                      : s === "Agent review"
                        ? "bg-violet-600 font-medium text-white"
                        : "bg-fg font-medium text-surface"
                    : i < at
                      ? "bg-sunken text-fg-2"
                      : "text-muted ring-1 ring-line ring-inset"
              }`}
            >
              {i < at && !skipped && "✓ "}
              {s}
            </span>
            {i < STAGES.length - 1 && <span className="text-faint">→</span>}
          </li>
        );
      })}
    </ol>
  );
}

function ClearedList({
  findings,
  open: opened = false,
}: {
  findings: FindingView[];
  open?: boolean;
}) {
  const [open, setOpen] = useState(opened);
  if (findings.length === 0) return null;
  return (
    <div className="rounded-lg border border-line bg-surface">
      <button
        type="button"
        onClick={() => setOpen(!open)}
        className="flex w-full items-center gap-2 px-4 py-2.5 text-left text-sm"
      >
        <span className="text-ok">✓</span>
        <span className="font-medium">
          The agent cleared {findings.length} {findings.length === 1 ? "thing" : "things"} on its
          own
        </span>
        <span className="ml-auto text-muted">{open ? "Hide" : "Show"}</span>
      </button>
      {open && (
        <ul className="border-t border-line-soft px-4 py-2 text-sm">
          {findings.map((f) => (
            <li key={f.id} className="flex items-center gap-3 py-1">
              <span className="text-ok">✓</span>
              <span className="flex-1 text-fg-2">
                <Md>{f.title}</Md>
              </span>
              <span className="text-xs text-faint tabular-nums">{pct(f.confidence)}</span>
            </li>
          ))}
        </ul>
      )}
    </div>
  );
}

/** The PR whose review an entry shows: the stack's layer it is held for, else its top. */
export const reviewedPr = (e: Gated & { number: number; layers: number[] }) =>
  e.hold?.layer ?? e.layers.at(-1) ?? e.number;

/** The gate in a queued PR's detail: the stages, and what the review waits for. */
export function Gate({
  workspace,
  repo,
  entry,
  onAnswer,
}: {
  workspace: number;
  repo: string;
  entry: Gated & { number: number; layers: number[] };
  onAnswer: () => void;
}) {
  const pr = reviewedPr(entry);
  const review = useAgentReview(workspace, repo, pr);
  const r = review.data;
  if (!r || r.status === "stale") return null;
  const reason = entry.state === "held" ? entry.hold?.reason : undefined;
  const asks = r.findings.filter((f) => f.asked);
  const back = r.findings.filter((f) => f.sent_back || f.withdrawn);
  const cleared = r.findings.filter((f) => !f.asked && !f.sent_back && !f.withdrawn);
  const human = r.answered_by || r.status === "asks" ? "ask" : "skip";
  const at =
    reason === "reviewing"
      ? 0
      : reason === "review_asks" || reason === "review_changes"
        ? 1
        : entry.state === "active"
          ? 3
          : 2;
  return (
    <section className="mt-5 space-y-4">
      <Stages at={at} human={human} />
      {reason === "reviewing" && (
        <p className="text-sm text-muted">
          The agent is reviewing it, since {timeAgo(r.created_at)}. What it finds to fix goes back
          to the author; a human is asked only about a call the author can't make.
        </p>
      )}
      {reason === "review_asks" && (
        <Asks workspace={workspace} repo={repo} pr={pr} review={r} onAnswer={onAnswer} />
      )}
      {r.status === "changes" && (
        <div className="rounded-lg border border-red-200 bg-red-50 px-4 py-3 text-red-900 dark:border-red-400/30 dark:bg-red-400/10 dark:text-red-200">
          <p className="font-medium">Sent back</p>
          <p className="mt-0.5">
            {r.answered_by
              ? `@${r.answered_by} asked for changes, in a review on GitHub.`
              : "The agent sent what it found back to the author, in a review on GitHub."}{" "}
            Each finding is a comment on its line. It keeps its place, and goes on once the author
            resolves every comment, fixed or answered; nobody needs to approve it. A push gets a new
            review, which reads the replies.
          </p>
        </div>
      )}
      {back.length > 0 && <SentBackList findings={back} />}
      {r.status === "approved" && r.answered_by && (
        <p className="text-sm text-muted">
          Approved by @{r.answered_by} on the agent's {asks.length}{" "}
          {asks.length === 1 ? "question" : "questions"}.
        </p>
      )}
      {r.status === "approved" && !r.answered_by && (
        <p className="text-sm text-muted">
          The agent approved it without asking anyone: it was sure enough about everything it found.
        </p>
      )}
      {r.answers.length > 0 && <Answered review={r} />}
      <ClearedList findings={cleared} />
    </section>
  );
}

/** What the agent sent back to the author: still to fix, or withdrawn on their reply. */
function SentBackList({ findings }: { findings: FindingView[] }) {
  return (
    <ul className="space-y-2 text-sm">
      {findings.map((f) => (
        <li key={f.id} className="rounded-md bg-surface px-3 py-2 ring-1 ring-line">
          <p className="flex items-center gap-2">
            <span className={f.withdrawn ? "text-muted" : "text-bad"}>
              {f.withdrawn ? "–" : "↩"}
            </span>
            <span className="font-mono text-xs text-faint">{f.id}</span>
            <span className={f.withdrawn ? "text-muted line-through" : ""}>
              <Md>{f.title}</Md>
            </span>
            <span className="ml-auto text-xs whitespace-nowrap text-faint">
              {f.withdrawn ? "withdrawn on the author's reply" : `${f.confidence}% sure`}
            </span>
          </p>
          {!f.withdrawn && f.suggestion && (
            <p className="mt-1 pl-5 text-muted">
              <Md>{f.suggestion}</Md>
            </p>
          )}
        </li>
      ))}
    </ul>
  );
}

function Answered({ review: r }: { review: AgentReviewView }) {
  return (
    <ul className="space-y-2 text-sm">
      {r.answers.map((a) => {
        const f = r.findings.find((f) => f.id === a.finding);
        return (
          <li key={a.finding} className="rounded-md bg-surface px-3 py-2 ring-1 ring-line">
            <p className="flex items-center gap-2">
              <span className={a.call === "change" ? "text-bad" : "text-ok"}>
                {a.call === "change" ? "✕" : "✓"}
              </span>
              <Md>{f?.title ?? a.finding}</Md>
            </p>
            {a.call === "change" && (
              <p className="mt-1 pl-5 text-muted">
                “<Md>{a.note}</Md>”
              </p>
            )}
          </li>
        );
      })}
    </ul>
  );
}

/** What the agent asks a human, with who it asked and a way to ask someone else. */
function Asks({
  workspace,
  repo,
  pr,
  review: r,
  onAnswer,
}: {
  workspace: number;
  repo: string;
  pr: number;
  review: AgentReviewView;
  onAnswer: () => void;
}) {
  const client = useQueryClient();
  const [picking, setPicking] = useState(false);
  const [logins, setLogins] = useState("");
  const asks = r.findings.filter((f) => f.asked);
  const sent = r.findings.filter((f) => f.sent_back).length;
  const reassign = useMutation({
    mutationFn: () =>
      askReviewers(
        workspace,
        repo,
        pr,
        logins.split(/[\s,]+/).filter((l) => l),
      ),
    onSuccess: (review) => {
      capture("agent review reassigned", { repo });
      client.setQueryData(["agent-review", workspace, repo, pr], review);
      setPicking(false);
      setLogins("");
    },
  });
  useKeys({ enter: onAnswer });
  return (
    <div className="rounded-lg border border-amber-200 bg-amber-50 px-4 py-3 text-amber-900 dark:border-amber-400/30 dark:bg-amber-400/10 dark:text-amber-200">
      {r.error ? (
        <>
          <p className="font-medium">The agent could not review it</p>
          <p className="mt-0.5">
            {r.error} A human reviews the whole PR instead: approve it, or ask for changes.
          </p>
        </>
      ) : (
        <>
          <p className="font-medium">
            The agent wants a human on {asks.length} {asks.length === 1 ? "thing" : "things"}
          </p>
          <p className="mt-0.5">
            {sent > 0
              ? `It sent ${sent} back to the author to fix, and cleared the other ${r.findings.length - asks.length - sent} itself.`
              : `It cleared the other ${r.findings.length - asks.length} itself.`}
          </p>
          <ul className="mt-3 space-y-1.5">
            {asks.map((f) => (
              <li key={f.id} className="flex items-center gap-2">
                <KindPill kind={f.kind} />
                <span className="truncate">
                  <Md>{f.title}</Md>
                </span>
                <span className="ml-auto shrink-0 text-xs tabular-nums opacity-70">
                  {pct(f.confidence)} sure
                </span>
              </li>
            ))}
          </ul>
        </>
      )}
      <div className="mt-4 flex flex-wrap items-center gap-2">
        <button type="button" className={`${primary} h-8 px-3`} onClick={onAnswer}>
          {r.error ? "Review it" : "Answer them"}{" "}
          <span className="ml-2 text-[10px] opacity-70">↵</span>
        </button>
        <button
          type="button"
          className={`${secondary} h-8 px-3`}
          onClick={() => setPicking(!picking)}
        >
          Ask someone else
        </button>
      </div>
      {picking && (
        <form
          className="mt-3 flex gap-2"
          onSubmit={(e) => {
            e.preventDefault();
            reassign.mutate();
          }}
        >
          <input
            value={logins}
            onChange={(e) => setLogins(e.target.value)}
            placeholder="GitHub logins, comma-separated"
            className={`${field} h-8 flex-1 text-fg`}
          />
          <button
            type="submit"
            disabled={!logins.trim() || reassign.isPending}
            className={`${secondary} h-8 px-3`}
          >
            Ask
          </button>
        </form>
      )}
      <ErrorText error={reassign.error} />
      <p className="mt-3 text-xs opacity-80">
        {r.asked.length
          ? `Asked ${people(r.asked)} on GitHub.`
          : "Anyone who may approve it can answer."}{" "}
        It keeps its place in the queue meanwhile.
      </p>
    </div>
  );
}

type Call = { call: "fine" } | { call: "change"; note: string };

/** The screen where a human answers the agent's review, one finding at a time. */
export function AgentReviewScreen({
  workspace,
  repo,
  pr,
  title,
  author,
  onExit,
}: {
  workspace: number;
  repo: string;
  pr: number;
  title: string;
  author: string | undefined;
  onExit: () => void;
}) {
  const review = useAgentReview(workspace, repo, pr);
  const r = review.data;
  if (review.isError) return <p className="p-6 text-bad">{review.error.message}</p>;
  if (!r) return <p className="p-6 text-faint">Loading the review…</p>;
  if (r.status !== "asks")
    return (
      <div className="p-6">
        <p className="text-muted">The review asks nothing now.</p>
        <button type="button" onClick={onExit} className={`${secondary} mt-3`}>
          Back to the queue
        </button>
      </div>
    );
  return (
    <Answering
      key={r.head_sha}
      workspace={workspace}
      repo={repo}
      pr={pr}
      title={title}
      author={author}
      review={r}
      onExit={onExit}
    />
  );
}

function Answering({
  workspace,
  repo,
  pr,
  title,
  author,
  review: r,
  onExit,
}: {
  workspace: number;
  repo: string;
  pr: number;
  title: string;
  author: string | undefined;
  review: AgentReviewView;
  onExit: () => void;
}) {
  const client = useQueryClient();
  const asks = r.findings.filter((f) => f.asked);
  const cleared = r.findings.filter((f) => !f.asked && !f.sent_back && !f.withdrawn);
  const [calls, setCalls] = useState<Record<string, Call>>({});
  const [at, setAt] = useState(0);
  const [note, setNote] = useState("");
  const [overall, setOverall] = useState("");
  const [sendBack, setSendBack] = useState(false);
  const [more, setMore] = useState(false);
  const [asking, setAsking] = useState("");
  const [composing, setComposing] = useState<"change" | "ask" | null>(null);
  const noteRef = useRef<HTMLTextAreaElement>(null);
  const summary = at >= asks.length;
  const f = asks[Math.min(at, asks.length - 1)];
  const decided = asks.filter((x) => calls[x.id]).length;
  const changes = asks.filter((x) => calls[x.id]?.call === "change").length;
  const sendsBack = asks.length ? changes > 0 : sendBack;
  const who = author ? `@${author}` : "the author";

  const submit = useMutation({
    mutationFn: () =>
      answerAgentReview(workspace, repo, pr, {
        head_sha: r.head_sha,
        answers: asks.map(
          (x): AnswerView => ({
            finding: x.id,
            call: calls[x.id].call,
            note: calls[x.id].call === "change" ? (calls[x.id] as { note: string }).note : "",
          }),
        ),
        note: overall,
        request_changes: sendBack,
      }),
    onSuccess: (review) => {
      capture("agent review answered", { repo, changes: sendsBack });
      client.setQueryData(["agent-review", workspace, repo, pr], review);
      client.invalidateQueries({ queryKey: ["lanes", workspace] });
      onExit();
    },
  });
  const ask = useMutation({
    mutationFn: (question: string) =>
      askAgent(workspace, repo, pr, { head_sha: r.head_sha, finding: f.id, question }),
    onSuccess: () => {
      setAsking("");
      setComposing(null);
      client.invalidateQueries({ queryKey: ["agent-review", workspace, repo, pr] });
    },
  });

  const move = (to: number) => {
    setAt(Math.max(0, Math.min(to, asks.length)));
    setNote("");
    setMore(false);
    setComposing(null);
  };
  const decide = (call: Call) => {
    const next = { ...calls, [f.id]: call };
    setCalls(next);
    // The next one not yet answered, else the summary.
    const i = asks.findIndex((x, i) => i > at && !next[x.id]);
    move(i === -1 ? asks.length : i);
  };
  const compose = (kind: "change" | "ask") => {
    setComposing(kind);
    requestAnimationFrame(() => document.getElementById(`review-${kind}`)?.focus());
  };
  useKeys({
    f: () => !summary && decide({ call: "fine" }),
    c: () => !summary && compose("change"),
    a: () => !summary && compose("ask"),
    j: () => move(at + 1),
    k: () => move(at - 1),
    m: () => setMore(!more),
    esc: () => (composing ? setComposing(null) : onExit()),
  });
  const questions = summary ? [] : r.questions.filter((q) => q.finding === f.id);

  return (
    <div className="flex h-full flex-col bg-page">
      <div className="flex items-center gap-4 border-b border-line bg-surface px-4 py-3 sm:px-6">
        <button type="button" onClick={onExit} className="shrink-0 text-muted hover:text-fg">
          ← Queue <Kbd>esc</Kbd>
        </button>
        <span className="text-faint max-sm:hidden">/</span>
        <p className="min-w-0 truncate font-medium max-sm:hidden">
          {title}{" "}
          <span className="font-normal text-muted">
            #{pr}
            {author && ` · ${author}`}
          </span>
        </p>
        <ol className="ml-auto flex shrink-0 items-center gap-1.5">
          {asks.map((x, i) => {
            const c = calls[x.id]?.call;
            return (
              <li key={x.id}>
                <button
                  type="button"
                  onClick={() => move(i)}
                  title={x.title}
                  aria-label={`Finding ${i + 1}`}
                  className={`h-1.5 w-8 rounded-full sm:w-10 ${
                    c === "fine"
                      ? "bg-emerald-500"
                      : c === "change"
                        ? "bg-red-500"
                        : i === at
                          ? "bg-fg"
                          : "bg-line-strong"
                  } ${i === at ? "ring-2 ring-fg/20 ring-offset-1 ring-offset-surface" : ""}`}
                />
              </li>
            );
          })}
          <li>
            <button
              type="button"
              onClick={() => move(asks.length)}
              className={`ml-1 text-xs ${summary ? "font-medium text-fg" : "text-muted hover:text-fg"}`}
            >
              {decided}/{asks.length} · Submit
            </button>
          </li>
        </ol>
      </div>

      {summary ? (
        <div className="min-h-0 flex-1 overflow-y-auto">
          <div className="mx-auto max-w-2xl px-6 pt-10 pb-16">
            <h2 className="text-2xl font-semibold tracking-tight">
              {decided < asks.length
                ? `${asks.length - decided} still to answer`
                : sendsBack
                  ? `Send back to ${who}`
                  : "Approve it into the queue"}
            </h2>
            <p className="mt-2 text-muted">
              {r.error
                ? `The agent could not review it: ${r.error} Review the whole PR, then approve it or ask for changes.`
                : decided < asks.length
                  ? "Answer every finding to submit."
                  : sendsBack
                    ? `Posts your review on GitHub asking for ${
                        changes === 1
                          ? "1 change, on its line"
                          : `${changes} changes, each on its line`
                      }. The PR keeps its place; on ${who}'s next push the agent reviews it again.`
                    : "Posts your approval on GitHub, and the PR takes its place in the queue."}
            </p>
            <ul className="mt-6 space-y-2">
              {asks.map((x, i) => {
                const c = calls[x.id];
                return (
                  <li key={x.id}>
                    <button
                      type="button"
                      onClick={() => move(i)}
                      className="flex w-full items-start gap-3 rounded-lg bg-surface px-4 py-3 text-left text-sm ring-1 ring-line hover:ring-line-strong"
                    >
                      <span
                        className={
                          c?.call === "change"
                            ? "text-bad"
                            : c?.call === "fine"
                              ? "text-ok"
                              : "text-warn"
                        }
                      >
                        {c?.call === "change" ? "✕" : c?.call === "fine" ? "✓" : "?"}
                      </span>
                      <span className="min-w-0 flex-1">
                        <span className="block">
                          <Md>{x.title}</Md>
                        </span>
                        <span className="mt-0.5 block text-muted">
                          {c?.call === "change" ? (
                            <>
                              “<Md>{c.note}</Md>”
                              {x.file && x.line && (
                                <span className="text-faint">
                                  {" "}
                                  · on {x.file.split("/").at(-1)}:{x.line}
                                </span>
                              )}
                            </>
                          ) : c?.call === "fine" ? (
                            "Fine as is"
                          ) : (
                            "Not answered"
                          )}
                        </span>
                      </span>
                      <span className="text-xs text-muted">Edit</span>
                    </button>
                  </li>
                );
              })}
            </ul>
            {asks.length === 0 && (
              <label className="mt-4 flex items-center gap-2 text-sm">
                <input
                  type="checkbox"
                  checked={sendBack}
                  onChange={(e) => setSendBack(e.target.checked)}
                />
                Ask for changes
              </label>
            )}
            <textarea
              value={overall}
              onChange={(e) => setOverall(e.target.value)}
              rows={2}
              placeholder={
                asks.length === 0 && sendBack
                  ? "What should change?"
                  : "Anything to add for the author? (optional)"
              }
              className="mt-4 w-full resize-none rounded-md border border-line-strong bg-surface px-3 py-2 text-sm outline-none placeholder:text-faint focus:border-fg"
            />
            <div className="mt-3 flex items-center gap-2">
              {cleared.length > 0 && (
                <span className="text-xs text-muted">
                  The agent cleared {cleared.length} more on its own
                </span>
              )}
              <button
                type="button"
                disabled={
                  decided < asks.length ||
                  submit.isPending ||
                  (asks.length === 0 && sendBack && !overall.trim())
                }
                onClick={() => submit.mutate()}
                className={`${primary} ml-auto ${
                  sendsBack
                    ? "bg-red-600! hover:bg-red-700!"
                    : "bg-emerald-600! hover:bg-emerald-700! dark:bg-emerald-500!"
                } text-white!`}
              >
                {submit.isPending
                  ? "Posting…"
                  : sendsBack
                    ? "Send back"
                    : "Approve, into the queue"}
              </button>
            </div>
            <ErrorText error={submit.error} />
          </div>
        </div>
      ) : (
        <div className="grid min-h-0 flex-1 lg:grid-cols-[1fr_400px]">
          <div className="min-h-0 overflow-y-auto p-4 sm:p-6">
            <p className="text-xs text-muted">
              Finding {at + 1} of {asks.length}
              {f.file && (
                <>
                  {" "}
                  · <span className="font-mono">{f.file}</span>
                </>
              )}
            </p>
            <h2 className="mt-1 text-xl font-semibold tracking-tight">
              <Md>{f.title}</Md>
            </h2>
            <p className="mt-2 max-w-3xl text-fg-2">
              <Md>{f.finding}</Md>
            </p>
            {f.file && (
              <div className="mt-4">
                <Code lines={f.code} file={f.line ? `${f.file}:${f.line}` : f.file} />
              </div>
            )}
            {f.related.length > 0 && (
              <>
                <button
                  type="button"
                  onClick={() => setMore(!more)}
                  className="mt-4 text-sm text-muted hover:text-fg"
                >
                  {more ? "▾" : "▸"} Other code it read ({f.related.length}) <Kbd>m</Kbd>
                </button>
                {more && (
                  <div className="mt-2 space-y-3">
                    {f.related.map((x) => (
                      <div key={`${x.file}:${x.line}`}>
                        <p className="mb-1 text-sm text-muted">
                          <Md>{x.why}</Md>
                        </p>
                        <Code lines={x.code} file={x.line ? `${x.file}:${x.line}` : x.file} />
                      </div>
                    ))}
                  </div>
                )}
              </>
            )}
          </div>
          <aside className="flex min-h-0 flex-col border-line bg-surface max-lg:border-t lg:border-l">
            <div className="min-h-0 flex-1 space-y-5 overflow-y-auto p-5 text-sm">
              <div className="flex flex-wrap items-center justify-between gap-2">
                <KindPill kind={f.kind} />
                <Confidence f={f} />
              </div>
              {f.unsure && (
                <div className="rounded-md bg-amber-50 px-3 py-2 text-amber-900 dark:bg-amber-400/10 dark:text-amber-200">
                  <h4 className="text-xs font-medium">Why it's asking you</h4>
                  <p className="mt-1">
                    <Md>{f.unsure}</Md>
                  </p>
                </div>
              )}
              {f.checked.length > 0 && (
                <div>
                  <h4 className="text-xs font-medium text-muted">Already checked, skip these</h4>
                  <ul className="mt-1 space-y-0.5 text-fg-2">
                    {f.checked.map((x) => (
                      <li key={x}>
                        <span className="text-ok">✓</span> <Md>{x}</Md>
                      </li>
                    ))}
                  </ul>
                </div>
              )}
              <div>
                <h4 className="text-xs font-medium text-muted">
                  It leans {f.lean === "fine" ? "fine as is" : "to a change"}
                </h4>
                {f.would_clear && (
                  <p className="mt-1 text-fg-2">
                    It would have decided alone given <Md>{f.would_clear}</Md>
                  </p>
                )}
              </div>
              {questions.map((q) => (
                <div key={q.id} className="rounded-md bg-sunken px-3 py-2">
                  <p className="text-xs text-muted">
                    @{q.by} asked: {q.question}
                  </p>
                  <p className="mt-1 text-fg-2">
                    {q.answer ? (
                      <Md>{q.answer}</Md>
                    ) : q.error ? (
                      <span className="text-bad">{q.error}</span>
                    ) : (
                      <span className="text-faint">The agent is looking into it…</span>
                    )}
                  </p>
                </div>
              ))}
            </div>
            <div className="space-y-2 border-t border-line p-4">
              {calls[f.id] && (
                <p className="text-xs text-muted">
                  Answered: {calls[f.id].call === "fine" ? "fine as is" : "needs a change"}. Pick
                  again to change it.
                </p>
              )}
              {composing === "change" ? (
                <div className="space-y-2">
                  <textarea
                    id="review-change"
                    ref={noteRef}
                    value={note}
                    onChange={(e) => setNote(e.target.value)}
                    onKeyDown={(e) => {
                      if (e.key === "Enter" && !e.shiftKey && note.trim()) {
                        e.preventDefault();
                        decide({ call: "change", note: note.trim() });
                      }
                    }}
                    rows={3}
                    placeholder={`What should ${author ?? "the author"} change?`}
                    className="w-full resize-none rounded-md border border-fg bg-surface px-3 py-2 text-sm outline-none ring-2 ring-fg/10 placeholder:text-faint"
                  />
                  <div className="flex flex-wrap items-center gap-2">
                    {f.suggestion && (
                      <button
                        type="button"
                        onClick={() => {
                          setNote(f.suggestion);
                          noteRef.current?.focus();
                        }}
                        className="text-xs text-muted underline underline-offset-2 hover:text-fg"
                      >
                        Use the agent's suggestion
                      </button>
                    )}
                    {f.file && f.line && (
                      <span className="text-xs text-faint">
                        posts on {f.file.split("/").at(-1)}:{f.line}
                      </span>
                    )}
                    <button
                      type="button"
                      disabled={!note.trim()}
                      onClick={() => decide({ call: "change", note: note.trim() })}
                      className={`${primary} ml-auto h-8 bg-red-600! px-3 text-white! hover:bg-red-700!`}
                    >
                      Needs a change ↵
                    </button>
                  </div>
                </div>
              ) : composing === "ask" ? (
                <form
                  onSubmit={(e) => {
                    e.preventDefault();
                    if (asking.trim()) ask.mutate(asking.trim());
                  }}
                >
                  <input
                    id="review-ask"
                    value={asking}
                    onChange={(e) => setAsking(e.target.value)}
                    placeholder="Ask the agent to dig further, then ↵"
                    disabled={ask.isPending}
                    className="h-9 w-full rounded-md border border-fg bg-surface px-3 text-sm outline-none ring-2 ring-fg/10 placeholder:text-faint"
                  />
                  <ErrorText error={ask.error} />
                </form>
              ) : (
                <>
                  <button
                    type="button"
                    onClick={() => decide({ call: "fine" })}
                    className={`${primary} w-full justify-between bg-emerald-600! text-white! hover:bg-emerald-700! dark:bg-emerald-500!`}
                  >
                    Fine as is <Kbd>f</Kbd>
                  </button>
                  <div className="grid grid-cols-2 gap-2">
                    <button
                      type="button"
                      onClick={() => compose("change")}
                      className={`${secondary} justify-between text-bad!`}
                    >
                      Needs a change <Kbd>c</Kbd>
                    </button>
                    <button
                      type="button"
                      onClick={() => compose("ask")}
                      className={`${secondary} justify-between`}
                    >
                      Ask the agent <Kbd>a</Kbd>
                    </button>
                  </div>
                </>
              )}
              <p className="flex justify-between pt-1 text-xs text-muted">
                <span>
                  <Kbd>k</Kbd> back · <Kbd>j</Kbd> skip
                </span>
                {cleared.length > 0 && (
                  <span>{cleared.length} cleared by the agent, not shown</span>
                )}
              </p>
            </div>
          </aside>
        </div>
      )}
    </div>
  );
}
