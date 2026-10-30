import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import { useNavigate, useSearch } from "@tanstack/react-router";
import { type ReactNode, useEffect, useRef, useState } from "react";
import { dequeuePull, fetchSession, fetchYou, queuePull, reviewPull } from "./api";
import type { InboxPullView } from "./api/InboxPullView";
import type { VerdictView } from "./api/VerdictView";
import { PullDetail } from "./PullDetail";
import { Avatar, primary, RepoIcon, secondary, timeAgo } from "./ui";

/** Needs you, then the user's own PRs by whose move it is. */
export const FOLDERS = ["needs", "turn", "queue", "others", "drafts"] as const;
export type Folder = (typeof FOLDERS)[number];
type Own = Exclude<Folder, "needs">;

const folderNames: Record<Folder, string> = {
  needs: "Needs you",
  turn: "Your turn",
  queue: "In the queue",
  others: "Waiting on others",
  drafts: "Drafts",
};

const keyOf = (p: InboxPullView) => `${p.repo}#${p.number}`;

/** GitHub's search lags a review by up to a minute or two: a reviewed PR stays out of Needs you
 * until it catches up. */
const REVIEWED_MS = 5 * 60_000;
/** The queue takes a label change within seconds. Past this, stop showing the click. */
const PENDING_MS = 60_000;

/** A click GitHub's search or the queue has not caught up with. */
type Click = { kind: "reviewed" | "queueing" | "dequeueing"; at: number };

/** Three panes, as the Queue page: the folders, the PRs in one, the selected PR with the review
 * box or its author's moves under it. Every repo of every workspace, read as the user. */
export function You({ workspace }: { workspace: number }) {
  const { folder = "needs", pr } = useSearch({ from: "/$account/you" });
  const navigate = useNavigate({ from: "/$account/you" });
  const me = useQuery({ queryKey: ["session"], queryFn: fetchSession }).data?.user?.login ?? "";
  const you = useQuery({ queryKey: ["you"], queryFn: fetchYou, refetchInterval: 30_000 });
  const client = useQueryClient();
  const [clicks, setClicks] = useState(new Map<string, Click>());
  const [error, setError] = useState<string | null>(null);
  const [toast, setToast] = useState<string | null>(null);
  const last = useRef(0);
  useEffect(() => {
    if (!toast) return;
    const t = setTimeout(() => setToast(null), 4000);
    return () => clearTimeout(t);
  }, [toast]);
  // Each click shows until the data has it, or for as long as it takes at most.
  const [now, setNow] = useState(Date.now());
  useEffect(() => {
    const t = setInterval(() => setNow(Date.now()), 5000);
    return () => clearInterval(t);
  }, []);
  const clickOf = (p: InboxPullView) => {
    const c = clicks.get(keyOf(p));
    if (!c) return undefined;
    const done =
      (c.kind === "reviewed" && now - c.at > REVIEWED_MS) ||
      (c.kind === "queueing" && (p.queue || now - c.at > PENDING_MS)) ||
      (c.kind === "dequeueing" && (!p.queue || now - c.at > PENDING_MS));
    return done ? undefined : c.kind;
  };
  const click = (p: InboxPullView, kind: Click["kind"] | null) =>
    setClicks((m) => {
      const next = new Map(m);
      if (kind) next.set(keyOf(p), { kind, at: Date.now() });
      else next.delete(keyOf(p));
      return next;
    });

  const review = useMutation({
    mutationFn: ({ p, verdict, body, queue }: ReviewArgs) =>
      reviewPull(p.workspace ?? workspace, p.repo, p.number, { verdict, body, queue }),
    onSuccess: (_, { p, verdict, queue }) => {
      // A comment leaves the review asked for: the PR stays for a verdict.
      if (verdict !== "comment") click(p, "reviewed");
      setToast(`${verdicts[verdict]} ${shortRef(p)}${queue ? " and queued it" : ""}`);
      client.invalidateQueries({ queryKey: ["you"] });
    },
    onError: (e) => setError(e.message),
  });
  const queue = useMutation({
    mutationFn: ({ p, priority }: { p: InboxPullView; priority: boolean }) =>
      queuePull(p.workspace ?? workspace, p.repo, p.number, priority),
    onMutate: ({ p }) => click(p, "queueing"),
    onSuccess: (_, { p, priority }) => {
      setToast(`${priority ? "Queued at the front:" : "Queued"} ${shortRef(p)}`);
      client.invalidateQueries({ queryKey: ["you"] });
    },
    onError: (e, { p }) => {
      click(p, null);
      setError(e.message);
    },
  });
  const dequeue = useMutation({
    mutationFn: (p: InboxPullView) => dequeuePull(p.workspace ?? workspace, p.repo, p.number),
    onMutate: (p) => click(p, "dequeueing"),
    onSuccess: (_, p) => {
      setToast(`Took ${shortRef(p)} out of the queue`);
      client.invalidateQueries({ queryKey: ["you"] });
    },
    onError: (e, p) => {
      click(p, null);
      setError(e.message);
    },
  });

  const data = you.data;
  const needs = (data?.review_requested ?? [])
    .filter((p) => clickOf(p) !== "reviewed")
    // The queue's first, then those back for another look; each updated last first.
    .sort((a, b) => reasonRank[reasonOf(a, me).kind] - reasonRank[reasonOf(b, me).kind]);
  const own = (data?.authored ?? []).map((p) => ({ p, step: stepOf(p, clickOf(p)) }));
  const lists: Record<Folder, InboxPullView[]> = {
    needs,
    turn: own.filter((o) => o.step.folder === "turn").map((o) => o.p),
    queue: own.filter((o) => o.step.folder === "queue").map((o) => o.p),
    others: own.filter((o) => o.step.folder === "others").map((o) => o.p),
    drafts: own.filter((o) => o.step.folder === "drafts").map((o) => o.p),
  };
  const list = lists[folder];
  let i = list.findIndex((p) => keyOf(p) === pr);
  // The selected PR left the folder, reviewed or queued: the one that took its place.
  if (i === -1) i = Math.min(last.current, list.length - 1);
  last.current = Math.max(i, 0);
  const pull = list[i];
  const picked = pull !== undefined && keyOf(pull) === pr;

  const select = (p?: InboxPullView) => navigate({ search: (s) => ({ ...s, pr: p && keyOf(p) }) });
  const open = (f: Folder) => {
    last.current = 0;
    navigate({ search: { folder: f } });
  };
  const approve = (p: InboxPullView, body = "", andQueue = false) =>
    review.mutate({ p, verdict: "approve", body, queue: andQueue });
  useKeys({
    j: () => select(list[Math.min(i + 1, list.length - 1)]),
    k: () => select(list[Math.max(i - 1, 0)]),
    a: () => pull && folder === "needs" && approve(pull),
    r: () => pull && folder === "needs" && document.getElementById("review-body")?.focus(),
    q: () =>
      pull &&
      folder !== "needs" &&
      stepOf(pull, clickOf(pull)).ready &&
      queue.mutate({ p: pull, priority: false }),
    ...Object.fromEntries(FOLDERS.map((f, n) => [String(n + 1), () => open(f)])),
  });

  if (you.isError)
    return <p className="p-6 text-bad">Could not load your PRs: {you.error.message}</p>;
  return (
    <div className="flex h-full flex-col lg:grid lg:grid-cols-[220px_minmax(360px,1fr)_minmax(400px,1.1fr)]">
      <nav
        className={`flex shrink-0 flex-col gap-0.5 border-line p-2 max-lg:flex-row max-lg:overflow-x-auto max-lg:border-b lg:overflow-y-auto lg:border-r ${picked ? "max-lg:hidden" : ""}`}
      >
        {FOLDERS.map((f, n) => (
          <div key={f} className="max-lg:shrink-0">
            {n === 1 && (
              <p className="mt-4 px-2 py-2 text-xs font-medium text-muted max-lg:hidden">
                Your PRs · {own.length}
              </p>
            )}
            <button
              type="button"
              onClick={() => open(f)}
              className={`flex w-full items-center justify-between gap-3 rounded-md px-2 py-1.5 text-left max-lg:w-auto ${
                folder === f ? "bg-surface shadow-xs ring-1 ring-line" : "hover:bg-sunken"
              }`}
            >
              <span className={n === 0 ? "font-medium" : "text-fg-2"}>{folderNames[f]}</span>
              {lists[f].length > 0 && (
                <span
                  className={`text-xs tabular-nums ${
                    f === "needs" || f === "turn"
                      ? "rounded-full bg-hot px-1.5 font-semibold text-white"
                      : "text-faint"
                  }`}
                >
                  {lists[f].length}
                </span>
              )}
            </button>
          </div>
        ))}
        <dl className="mt-auto grid grid-cols-[auto_1fr] items-center gap-x-2 gap-y-1.5 px-2 py-3 text-xs text-muted max-lg:hidden">
          <dt>
            <Kbd>j</Kbd> <Kbd>k</Kbd>
          </dt>
          <dd>move</dd>
          <dt>
            <Kbd>a</Kbd>
          </dt>
          <dd>approve</dd>
          <dt>
            <Kbd>r</Kbd>
          </dt>
          <dd>write a review</dd>
          <dt>
            <Kbd>q</Kbd>
          </dt>
          <dd>queue yours</dd>
          <dt>
            <Kbd>1</Kbd>–<Kbd>5</Kbd>
          </dt>
          <dd>folders</dd>
        </dl>
      </nav>

      <section
        className={`min-h-0 flex-1 overflow-y-auto border-line bg-surface lg:border-r ${picked ? "max-lg:hidden" : ""}`}
      >
        <div className="sticky top-0 z-10 border-b border-line bg-surface px-4 py-3">
          <p className="font-medium">{folderNames[folder]}</p>
          <p className="text-xs text-muted">
            {folder === "needs"
              ? "PRs that ask for your review, in every repo. Those the queue holds come first."
              : "Your open PRs, in every repo."}
          </p>
        </div>
        {error && (
          <p className="flex items-start justify-between gap-3 border-b border-red-200 bg-red-50 px-4 py-2 text-red-800 dark:border-red-400/30 dark:bg-red-400/10 dark:text-red-200">
            {error}
            <button
              type="button"
              onClick={() => setError(null)}
              className="shrink-0 font-medium hover:opacity-70"
            >
              Dismiss
            </button>
          </p>
        )}
        {you.isPending && <p className="p-4 text-faint">Loading…</p>}
        {data && list.length === 0 && (
          <div className="px-6 py-16 text-center">
            <p className="font-medium">
              {folder === "needs" ? "Nothing waits on your review" : "Nothing here"}
            </p>
            {folder !== "turn" && lists.turn.length > 0 && (
              <button
                type="button"
                onClick={() => open("turn")}
                className="mt-1 text-muted underline underline-offset-2 hover:text-fg"
              >
                {lists.turn.length} of your PRs need a move
              </button>
            )}
          </div>
        )}
        <Groups folder={folder} list={list} me={me}>
          {(p) => (
            <Row
              key={keyOf(p)}
              pull={p}
              selected={p === pull}
              line={
                folder === "needs" ? (
                  <Reason pull={p} me={me} />
                ) : (
                  <StepLine step={stepOf(p, clickOf(p))} />
                )
              }
              onClick={() => select(p)}
            />
          )}
        </Groups>
      </section>

      <section className={`min-h-0 flex-1 ${picked ? "" : "max-lg:hidden"}`}>
        {pull && (
          <PullDetail
            key={keyOf(pull)}
            workspace={pull.workspace ?? workspace}
            repo={pull.repo}
            trunk={pull.base}
            pull={pull}
            onBack={() => select()}
            footer={
              folder === "needs" ? (
                <ReviewBox
                  key={keyOf(pull)}
                  pull={pull}
                  busy={review.isPending}
                  onReview={(verdict, body, andQueue, sent) =>
                    review.mutate({ p: pull, verdict, body, queue: andQueue }, { onSuccess: sent })
                  }
                />
              ) : (
                <OwnerBar
                  pull={pull}
                  step={stepOf(pull, clickOf(pull))}
                  busy={!!clickOf(pull)}
                  onQueue={(priority) => queue.mutate({ p: pull, priority })}
                  onDequeue={() => dequeue.mutate(pull)}
                />
              )
            }
          />
        )}
      </section>
      {toast && (
        <div className="fixed bottom-4 left-4 z-50 flex items-center gap-3 rounded-lg bg-neutral-900 px-4 py-3 text-sm text-white shadow-xl dark:bg-neutral-100 dark:text-neutral-900">
          <span className="size-2 rounded-full bg-emerald-400" />
          {toast}
        </div>
      )}
    </div>
  );
}

type ReviewArgs = { p: InboxPullView; verdict: VerdictView; body: string; queue: boolean };

const verdicts: Record<VerdictView, string> = {
  approve: "Approved",
  request_changes: "Requested changes on",
  comment: "Commented on",
};

const shortRef = (p: InboxPullView) => `${p.repo.split("/")[1]}#${p.number}`;

// ── Why a PR needs you ───────────────────────────────────────────────────────

type Reason = { kind: "held" | "again" | "asked"; text: string };
const reasonRank: Record<Reason["kind"], number> = { held: 0, again: 1, asked: 2 };

function reasonOf(p: InboxPullView, me: string): Reason {
  if (p.queue?.state === "held")
    return { kind: "held", text: `Held in the queue: ${p.queue.detail ?? "waiting"}` };
  const mine = p.reviews.find((r) => r.reviewer.login === me);
  if (mine)
    return {
      kind: "again",
      text: mine.approved
        ? "Back for another look since you approved"
        : "Back for another look since you requested changes",
    };
  return { kind: "asked", text: `${p.author?.login ?? "ghost"} asked ${timeAgo(p.updated_at)}` };
}

const reasonGroups: Record<Reason["kind"], string> = {
  held: "Holding up the queue",
  again: "Back for another look",
  asked: "Review requested",
};

const reasonStyle: Record<Reason["kind"], string> = {
  held: "text-hot",
  again: "text-blue-600 dark:text-blue-400",
  asked: "text-muted",
};

function Reason({ pull, me }: { pull: InboxPullView; me: string }) {
  const r = reasonOf(pull, me);
  return <span className={`truncate ${reasonStyle[r.kind]}`}>{r.text}</span>;
}

/** Needs you in groups by why, each with a header. Any other folder as one list. */
function Groups({
  folder,
  list,
  me,
  children,
}: {
  folder: Folder;
  list: InboxPullView[];
  me: string;
  children: (p: InboxPullView) => ReactNode;
}) {
  if (folder !== "needs")
    return (
      <ol>
        {list.map((p) => (
          <li key={keyOf(p)}>{children(p)}</li>
        ))}
      </ol>
    );
  return (["held", "again", "asked"] as const).map((kind) => {
    const ps = list.filter((p) => reasonOf(p, me).kind === kind);
    if (!ps.length) return null;
    return (
      <div key={kind}>
        <p className="border-b border-line-soft bg-sunken px-4 py-1.5 text-xs font-medium text-muted">
          {reasonGroups[kind]} · {ps.length}
        </p>
        <ol>
          {ps.map((p) => (
            <li key={keyOf(p)}>{children(p)}</li>
          ))}
        </ol>
      </div>
    );
  });
}

// ── What the author's next move is ───────────────────────────────────────────

type Step = { folder: Own; label: string; detail?: string; ready?: boolean };

const ordinal = (n: number) =>
  `${n}${["th", "st", "nd", "rd"][n % 100 >= 11 && n % 100 <= 13 ? 0 : n % 10 > 3 ? 0 : n % 10]}`;

/** Where the author's PR stands, and so its folder. `click`: a queue click not caught up with. */
function stepOf(p: InboxPullView, click?: Click["kind"]): Step {
  if (click === "queueing") return { folder: "queue", label: "Queueing…" };
  if (click === "dequeueing") return { folder: "queue", label: "Leaving the queue…" };
  const q = p.queue;
  if (q?.state === "active")
    return { folder: "queue", label: "Testing", detail: "merges if its checks pass" };
  if (q?.state === "waiting")
    return {
      folder: "queue",
      label: `${ordinal(q.place)} of ${q.of} in line`,
      detail: q.priority ? "priority" : undefined,
    };
  if (q?.state === "held")
    return { folder: "turn", label: "Held in the queue", detail: q.detail ?? undefined };
  if (p.draft) return { folder: "drafts", label: "Draft" };
  if (p.review_decision === "changes_requested") {
    const who = p.reviews.filter((r) => !r.approved).map((r) => r.reviewer.login);
    return {
      folder: "turn",
      label: "Changes requested",
      detail: who.length ? `by ${who.join(", ")}` : undefined,
    };
  }
  if (p.checks === "failed") return { folder: "turn", label: "Checks failing" };
  if (p.review_decision === "review_required")
    return {
      folder: "others",
      label: "Waiting on a review",
      detail: p.requested.length ? `from ${p.requested.map((r) => r.login).join(", ")}` : undefined,
    };
  if (p.checks === "pending") return { folder: "others", label: "Checks running" };
  return p.queues
    ? { folder: "turn", label: "Ready to queue", ready: true }
    : { folder: "turn", label: "Ready to merge", detail: "Mergepatrol is not on in this repo" };
}

function StepLine({ step }: { step: Step }) {
  return (
    <span
      className={`truncate ${
        step.folder === "turn"
          ? "text-hot"
          : step.folder === "queue"
            ? "text-blue-600 dark:text-blue-400"
            : "text-muted"
      }`}
    >
      {step.label}
      {step.detail && <span className="text-muted"> · {step.detail}</span>}
    </span>
  );
}

// ── Rows and bars ────────────────────────────────────────────────────────────

function Row({
  pull: p,
  selected,
  line,
  onClick,
}: {
  pull: InboxPullView;
  selected: boolean;
  line: ReactNode;
  onClick: () => void;
}) {
  return (
    <button
      type="button"
      onClick={onClick}
      className={`flex w-full flex-col gap-1 border-b border-line-soft px-4 py-3 text-left ${
        selected ? "lg:bg-page lg:shadow-[inset_2px_0_0_0_var(--color-fg)]" : "hover:bg-page"
      }`}
    >
      <span className="flex items-center justify-between gap-3 text-xs text-muted">
        <span className="flex min-w-0 items-center gap-1.5">
          <RepoIcon repo={p.repo} size="size-4" />
          <span className="truncate">
            {p.repo} <span className="tabular-nums">#{p.number}</span>
          </span>
        </span>
        <span className="shrink-0 text-faint">{timeAgo(p.updated_at)}</span>
      </span>
      <span className="truncate font-medium">{p.title}</span>
      <span className="flex items-center gap-2 text-xs">
        {line}
        <span className="ml-auto flex shrink-0 items-center gap-2">
          <Reviewers pull={p} />
          <Checks pull={p} />
          <span className="font-mono tabular-nums">
            <span className="text-ok">+{p.additions}</span>{" "}
            <span className="text-bad">−{p.deletions}</span>
          </span>
        </span>
      </span>
    </button>
  );
}

function Reviewers({ pull: p }: { pull: InboxPullView }) {
  return (
    <span className="flex items-center gap-1">
      {p.reviews.map((r) => (
        <span
          key={r.reviewer.login}
          className="relative"
          title={`${r.reviewer.login} ${r.approved ? "approved" : "requested changes"}`}
        >
          <Avatar src={r.reviewer.avatar_url} alt={r.reviewer.login} size="size-4" />
          <span
            className={`absolute -right-1 -bottom-1 text-[9px] font-bold ${r.approved ? "text-ok" : "text-bad"}`}
          >
            {r.approved ? "✓" : "✕"}
          </span>
        </span>
      ))}
      {p.requested.map((r) => (
        <span key={r.login} className="opacity-40" title={`${r.login} has not reviewed yet`}>
          <Avatar src={r.avatar_url} alt={r.login} size="size-4" />
        </span>
      ))}
    </span>
  );
}

const checkMarks = {
  passed: ["✓", "text-ok", "Checks passed"],
  failed: ["✕", "text-bad", "Checks failed"],
  pending: ["●", "text-warn", "Checks running"],
} as const;

function Checks({ pull: p }: { pull: InboxPullView }) {
  if (!p.checks) return null;
  const [mark, color, title] = checkMarks[p.checks];
  return (
    <span className={color} title={title}>
      {mark}
    </span>
  );
}

/** Approve, request changes or comment, as the user. ⌘↵ approves. A change request or a comment
 * needs words. */
function ReviewBox({
  pull: p,
  busy,
  onReview,
}: {
  pull: InboxPullView;
  busy: boolean;
  /** `sent` once GitHub has the review: the box keeps its words until then. */
  onReview: (verdict: VerdictView, body: string, andQueue: boolean, sent: () => void) => void;
}) {
  const [body, setBody] = useState("");
  const [needsWords, setNeedsWords] = useState(false);
  const ref = useRef<HTMLTextAreaElement>(null);
  const send = (verdict: VerdictView, andQueue = false) => {
    if (verdict !== "approve" && !body.trim()) {
      setNeedsWords(true);
      ref.current?.focus();
      return;
    }
    // Words typed while it was sending stay.
    onReview(verdict, body, andQueue, () => setBody((b) => (b === body ? "" : b)));
  };
  const canQueue = !p.queue && p.queues && !p.draft;
  return (
    <div className="border-t border-line bg-surface px-4 py-3 sm:px-6">
      <textarea
        id="review-body"
        ref={ref}
        value={body}
        onChange={(e) => {
          setBody(e.target.value);
          setNeedsWords(false);
        }}
        onKeyDown={(e) => {
          if (e.key === "Enter" && (e.metaKey || e.ctrlKey)) {
            e.preventDefault();
            send("approve");
          }
          if (e.key === "Escape") e.currentTarget.blur();
        }}
        rows={2}
        placeholder="Leave a review comment…"
        className={`w-full resize-none rounded-md border bg-page px-3 py-2 text-sm outline-none placeholder:text-faint focus:ring-2 focus:ring-fg/10 ${
          needsWords ? "border-bad" : "border-line-strong focus:border-fg"
        }`}
      />
      {needsWords && (
        <p className="mt-1 text-xs text-bad">Say what to change, or what you think.</p>
      )}
      <div className="mt-2 flex flex-wrap items-center gap-2">
        <button
          type="button"
          disabled={busy}
          onClick={() => send("request_changes")}
          className={`${secondary} h-8 px-3 text-bad!`}
        >
          Request changes
        </button>
        <button
          type="button"
          disabled={busy}
          onClick={() => send("comment")}
          className={`${secondary} h-8 px-3`}
        >
          Comment
        </button>
        <span className="ml-auto flex items-center gap-2">
          {p.queue?.state === "held" && (
            <span className="text-xs text-muted max-sm:hidden">
              The queue holds it: your approval may let it go
            </span>
          )}
          {canQueue && (
            <button
              type="button"
              disabled={busy}
              onClick={() => send("approve", true)}
              className={`${secondary} h-8 px-3`}
            >
              Approve & queue
            </button>
          )}
          <button
            type="button"
            disabled={busy}
            onClick={() => send("approve")}
            className={`${primary} h-8 bg-emerald-600 px-3 text-white hover:bg-emerald-700 dark:bg-emerald-500 dark:hover:bg-emerald-400`}
          >
            Approve <span className="ml-2 text-[10px] opacity-70">⌘↵</span>
          </button>
        </span>
      </div>
    </div>
  );
}

/** The author's moves on their own PR: queue it, or take it out. */
function OwnerBar({
  pull: p,
  step,
  busy,
  onQueue,
  onDequeue,
}: {
  pull: InboxPullView;
  step: Step;
  busy: boolean;
  onQueue: (priority: boolean) => void;
  onDequeue: () => void;
}) {
  return (
    <div className="flex flex-wrap items-center gap-2 border-t border-line bg-surface px-4 py-3 sm:px-6">
      <span className="min-w-0 flex-1 text-sm">
        <span className="font-medium">{step.label}</span>
        {step.detail && <span className="block truncate text-xs text-muted">{step.detail}</span>}
      </span>
      {p.queue ? (
        <>
          {!p.queue.priority && (
            <button
              type="button"
              disabled={busy}
              onClick={() => onQueue(true)}
              title="Move to the front"
              className={`${secondary} h-8 px-3 text-hot!`}
            >
              ↑ Priority
            </button>
          )}
          <button
            type="button"
            disabled={busy}
            onClick={onDequeue}
            className={`${secondary} h-8 px-3`}
          >
            Dequeue
          </button>
        </>
      ) : (
        p.queues &&
        !p.draft && (
          <>
            <button
              type="button"
              disabled={busy}
              onClick={() => onQueue(true)}
              title="Queue at the front"
              className={`${secondary} h-8 px-3 text-hot!`}
            >
              ↑ Priority
            </button>
            <button
              type="button"
              disabled={busy}
              onClick={() => onQueue(false)}
              className={step.ready ? `${primary} h-8 px-3` : `${secondary} h-8 px-3`}
            >
              Queue
            </button>
          </>
        )
      )}
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

/** Single keys, off while typing or with a modifier. */
function useKeys(map: Record<string, () => unknown>) {
  useEffect(() => {
    const onKey = (e: KeyboardEvent) => {
      if ((e.target as HTMLElement).closest("input, textarea, select, [contenteditable]")) return;
      if (e.metaKey || e.ctrlKey || e.altKey) return;
      const f = map[e.key];
      if (f) {
        e.preventDefault();
        f();
      }
    };
    addEventListener("keydown", onKey);
    return () => removeEventListener("keydown", onKey);
  });
}
