import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import { useNavigate, useSearch } from "@tanstack/react-router";
import { type ReactNode, useEffect, useReducer, useRef, useState } from "react";
import {
  dequeuePull,
  fetchSession,
  fetchYou,
  nudgePull,
  queuePull,
  reviewPull,
  reviewUrl,
  seenPull,
} from "./api";
import type { InboxPullView } from "./api/InboxPullView";
import type { ReviewPull } from "./api/ReviewPull";
import type { VerdictView } from "./api/VerdictView";
import { PullDetail } from "./PullDetail";
import { Avatar, primary, RepoIcon, secondary, timeAgo } from "./ui";

/** Needs you, then the user's own PRs by whose move it is, then those merged lately. */
export const FOLDERS = ["needs", "turn", "queue", "others", "drafts", "merged"] as const;
export type Folder = (typeof FOLDERS)[number];
type Own = Exclude<Folder, "needs" | "merged">;

const folderNames: Record<Folder, string> = {
  needs: "Needs you",
  turn: "Your turn",
  queue: "In the queue",
  others: "Waiting on others",
  drafts: "Drafts",
  merged: "Merged",
};

const keyOf = (p: InboxPullView) => `${p.repo}#${p.number}`;

/** A review waits this long before it goes to GitHub, which cannot take one back: Undo until
 * then. */
const HOLD_MS = 5000;
/** The queue takes a label change within seconds. Past this, stop showing the click. */
const PENDING_MS = 60_000;

/** A queue click the queue has not caught up with. */
type Click = { kind: "queueing" | "dequeueing"; at: number };

/** A review waiting out `HOLD_MS`, or on its way to GitHub. */
type Held = { p: InboxPullView; review: ReviewPull; until: number; timer: number; sending?: true };

/** Three panes, as the Queue page: the folders, the PRs in one, the selected PR with the review
 * box or its author's moves under it. Every repo of every workspace, as webhooks keep them. */
export function You() {
  const { folder = "needs", pr } = useSearch({ from: "/$account/you" });
  const navigate = useNavigate({ from: "/$account/you" });
  const me = useQuery({ queryKey: ["session"], queryFn: fetchSession }).data?.user?.login ?? "";
  // Read from Mergepatrol's own database, not GitHub: cheap to read often.
  const you = useQuery({ queryKey: ["you"], queryFn: fetchYou, refetchInterval: 5000 });
  const client = useQueryClient();
  const [clicks, setClicks] = useState(new Map<string, Click>());
  const [drafts, setDrafts] = useState(new Map<string, string>());
  const [error, setError] = useState<string | null>(null);
  const [toast, setToast] = useState<string | null>(null);
  const last = useRef(0);
  useEffect(() => {
    if (!toast) return;
    const t = setTimeout(() => setToast(null), 4000);
    return () => clearTimeout(t);
  }, [toast]);
  const [now, setNow] = useState(Date.now());
  const held = useRef(new Map<string, Held>());
  const [, render] = useReducer((n: number) => n + 1, 0);
  // Ticks the countdown while a review waits, and each click until the data has it.
  useEffect(() => {
    const t = setInterval(() => setNow(Date.now()), held.current.size ? 250 : 5000);
    return () => clearInterval(t);
  });
  const clickOf = (p: InboxPullView) => {
    const c = clicks.get(keyOf(p));
    if (!c) return undefined;
    const done =
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
  const setDraft = (key: string, text: string) =>
    setDrafts((d) => {
      const next = new Map(d);
      if (text) next.set(key, text);
      else next.delete(key);
      return next;
    });

  /** Sends a held review now. Its PR stays out of Needs you until the inbox has the review. */
  const send = async (key: string, h: Held) => {
    if (h.sending) return;
    h.sending = true;
    clearTimeout(h.timer);
    render();
    try {
      await reviewPull(h.p.workspace, h.p.repo, h.p.number, h.review);
      await client.refetchQueries({ queryKey: ["you"] });
      setToast(
        `${verdicts[h.review.verdict]} ${shortRef(h.p)}${h.review.queue ? " and queued it" : ""}`,
      );
    } catch (e) {
      setError((e as Error).message);
      setDraft(key, h.review.body);
    } finally {
      // A review held for the PR since is not this one's to drop.
      if (held.current.get(key) === h) held.current.delete(key);
      render();
    }
  };
  const hold = (p: InboxPullView, review: ReviewPull) => {
    const key = keyOf(p);
    // A review already waiting on the PR, a comment say, goes now.
    const waiting = held.current.get(key);
    if (waiting) send(key, waiting);
    const h: Held = { p, review, until: Date.now() + HOLD_MS, timer: 0 };
    h.timer = window.setTimeout(() => send(key, h), HOLD_MS);
    held.current.set(key, h);
    setDraft(key, "");
    render();
  };
  const undo = (key: string) => {
    const h = held.current.get(key);
    if (!h || h.sending) return;
    clearTimeout(h.timer);
    held.current.delete(key);
    setDraft(key, h.review.body);
    navigate({ search: { folder: "needs", pr: key } });
    render();
  };
  // Leaving the page sends what waits: a closing tab, or another tab of the dashboard.
  useEffect(() => {
    const flush = () => {
      for (const [key, h] of held.current)
        if (!h.sending) {
          clearTimeout(h.timer);
          held.current.delete(key);
          fetch(reviewUrl(h.p.workspace, h.p.repo, h.p.number), {
            method: "POST",
            headers: { "content-type": "application/json" },
            body: JSON.stringify(h.review),
            keepalive: true,
          });
        }
    };
    addEventListener("pagehide", flush);
    return () => {
      removeEventListener("pagehide", flush);
      flush();
    };
  }, []);

  const queue = useMutation({
    mutationFn: ({ p, priority }: { p: InboxPullView; priority: boolean }) =>
      queuePull(p.workspace, p.repo, p.number, priority),
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
    mutationFn: (p: InboxPullView) => dequeuePull(p.workspace, p.repo, p.number),
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
  const nudge = useMutation({
    mutationFn: (p: InboxPullView) =>
      nudgePull(
        p.workspace,
        p.repo,
        p.number,
        p.requested.map((r) => r.login),
      ),
    onSuccess: (_, p) =>
      setToast(`Asked ${p.requested.map((r) => r.login).join(", ")} to review ${shortRef(p)}`),
    onError: (e) => setError(e.message),
  });

  const data = you.data;
  // A review waiting or on its way takes its PR out, but a comment, which answers nothing.
  const reviewing = (p: InboxPullView) => {
    const h = held.current.get(keyOf(p));
    return h !== undefined && h.review.verdict !== "comment";
  };
  const needs = (data?.review_requested ?? [])
    .filter((p) => !reviewing(p))
    // The queue's first, then those back for another look; each updated last first.
    .sort((a, b) => reasonRank[reasonOf(a, me).kind] - reasonRank[reasonOf(b, me).kind]);
  const own = (data?.authored ?? []).map((p) => ({ p, step: stepOf(p, clickOf(p)) }));
  const lists: Record<Folder, InboxPullView[]> = {
    needs,
    turn: own.filter((o) => o.step.folder === "turn").map((o) => o.p),
    queue: own.filter((o) => o.step.folder === "queue").map((o) => o.p),
    others: own.filter((o) => o.step.folder === "others").map((o) => o.p),
    drafts: own.filter((o) => o.step.folder === "drafts").map((o) => o.p),
    merged: data?.merged ?? [],
  };
  const list = lists[folder];
  let i = list.findIndex((p) => keyOf(p) === pr);
  // The selected PR left the folder, reviewed or queued: the one that took its place.
  if (i === -1) i = Math.min(last.current, list.length - 1);
  last.current = Math.max(i, 0);
  const pull = list[i];
  const picked = pull !== undefined && keyOf(pull) === pr;

  // Opened here: what was said on it is read.
  const unread = pull?.unread ?? 0;
  const pullKey = pull && keyOf(pull);
  // biome-ignore lint/correctness/useExhaustiveDependencies: once per PR opened with news.
  useEffect(() => {
    if (!pull || !unread) return;
    seenPull(pull.workspace, pull.repo, pull.number).then(
      () => client.invalidateQueries({ queryKey: ["you"] }),
      () => {},
    );
  }, [pullKey, unread]);

  const select = (p?: InboxPullView) => navigate({ search: (s) => ({ ...s, pr: p && keyOf(p) }) });
  const open = (f: Folder) => {
    last.current = 0;
    navigate({ search: { folder: f } });
  };
  const newest = [...held.current.keys()].at(-1);
  useKeys({
    j: () => select(list[Math.min(i + 1, list.length - 1)]),
    k: () => select(list[Math.max(i - 1, 0)]),
    a: () =>
      pull &&
      folder === "needs" &&
      hold(pull, { verdict: "approve", body: drafts.get(keyOf(pull)) ?? "", queue: false }),
    r: () => pull && folder === "needs" && document.getElementById("review-body")?.focus(),
    u: () => newest && undo(newest),
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
            <Kbd>u</Kbd>
          </dt>
          <dd>undo a review</dd>
          <dt>
            <Kbd>q</Kbd>
          </dt>
          <dd>queue yours</dd>
          <dt>
            <Kbd>1</Kbd>–<Kbd>6</Kbd>
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
              ? "PRs that ask for your review, or your team's, in every repo. Those the queue holds come first."
              : folder === "merged"
                ? "Yours, merged in the last week."
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
                ) : folder === "merged" ? (
                  <span className="text-ok">Merged {p.merged_at && timeAgo(p.merged_at)}</span>
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
            // Read again from GitHub only when a webhook said the PR changed.
            version={`${pull.updated_at} ${pull.checks} ${pull.review_decision}`}
            workspace={pull.workspace}
            repo={pull.repo}
            trunk={pull.base}
            pull={pull}
            onBack={() => select()}
            footer={
              folder === "needs" ? (
                <ReviewBox
                  pull={pull}
                  body={drafts.get(keyOf(pull)) ?? ""}
                  onBody={(text) => setDraft(keyOf(pull), text)}
                  onReview={(review) => hold(pull, review)}
                />
              ) : folder === "merged" ? null : (
                <OwnerBar
                  pull={pull}
                  step={stepOf(pull, clickOf(pull))}
                  busy={!!clickOf(pull)}
                  onQueue={(priority) => queue.mutate({ p: pull, priority })}
                  onDequeue={() => dequeue.mutate(pull)}
                  onNudge={nudge.isPending ? undefined : () => nudge.mutate(pull)}
                />
              )
            }
          />
        )}
      </section>
      <div className="fixed bottom-4 left-4 z-50 flex flex-col gap-2">
        {[...held.current].map(([key, h]) => (
          <div
            key={key}
            className="flex items-center gap-3 rounded-lg bg-neutral-900 px-4 py-3 text-sm text-white shadow-xl dark:bg-neutral-100 dark:text-neutral-900"
          >
            <span
              className={`size-2 rounded-full ${h.review.verdict === "request_changes" ? "bg-red-400" : "bg-emerald-400"}`}
            />
            <span>
              {holding[h.review.verdict]} {shortRef(h.p)}
              {h.sending ? "…" : ` in ${Math.max(1, Math.ceil((h.until - now) / 1000))}s`}
            </span>
            {!h.sending && (
              <>
                <button
                  type="button"
                  onClick={() => undo(key)}
                  className="font-medium underline underline-offset-2 hover:opacity-80"
                >
                  Undo
                </button>
                <button
                  type="button"
                  onClick={() => send(key, h)}
                  className="opacity-60 hover:opacity-100"
                  title="Send it now"
                >
                  Send now
                </button>
              </>
            )}
          </div>
        ))}
        {toast && (
          <div className="flex items-center gap-3 rounded-lg bg-neutral-900 px-4 py-3 text-sm text-white shadow-xl dark:bg-neutral-100 dark:text-neutral-900">
            <span className="size-2 rounded-full bg-emerald-400" />
            {toast}
          </div>
        )}
      </div>
    </div>
  );
}

const verdicts: Record<VerdictView, string> = {
  approve: "Approved",
  request_changes: "Requested changes on",
  comment: "Commented on",
};

const holding: Record<VerdictView, string> = {
  approve: "Approving",
  request_changes: "Requesting changes on",
  comment: "Commenting on",
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
        <span className="flex shrink-0 items-center gap-2 text-faint">
          {p.unread > 0 && (
            <span
              className="rounded-full bg-blue-600 px-1.5 text-[11px] leading-[18px] font-semibold text-white tabular-nums"
              title={`${p.unread} new ${p.unread === 1 ? "comment or review" : "comments and reviews"}`}
            >
              {p.unread}
            </span>
          )}
          {timeAgo(p.updated_at)}
        </span>
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
 * needs words. The words stay with the PR, kept by the page, until the review goes. */
function ReviewBox({
  pull: p,
  body,
  onBody,
  onReview,
}: {
  pull: InboxPullView;
  body: string;
  onBody: (body: string) => void;
  onReview: (review: ReviewPull) => void;
}) {
  const [needsWords, setNeedsWords] = useState(false);
  const ref = useRef<HTMLTextAreaElement>(null);
  const send = (verdict: VerdictView, queue = false) => {
    if (verdict !== "approve" && !body.trim()) {
      setNeedsWords(true);
      ref.current?.focus();
      return;
    }
    onReview({ verdict, body, queue });
  };
  const canQueue = !p.queue && p.queues && !p.draft;
  return (
    <div className="border-t border-line bg-surface px-4 py-3 sm:px-6">
      <textarea
        id="review-body"
        ref={ref}
        value={body}
        onChange={(e) => {
          onBody(e.target.value);
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
          onClick={() => send("request_changes")}
          className={`${secondary} h-8 px-3 text-bad!`}
        >
          Request changes
        </button>
        <button type="button" onClick={() => send("comment")} className={`${secondary} h-8 px-3`}>
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
              onClick={() => send("approve", true)}
              className={`${secondary} h-8 px-3`}
            >
              Approve & queue
            </button>
          )}
          <button
            type="button"
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
  onNudge,
}: {
  pull: InboxPullView;
  step: Step;
  busy: boolean;
  onQueue: (priority: boolean) => void;
  onDequeue: () => void;
  /** Mentions the reviewers who have not answered. `undefined` while it goes. */
  onNudge?: () => void;
}) {
  return (
    <div className="flex flex-wrap items-center gap-2 border-t border-line bg-surface px-4 py-3 sm:px-6">
      <span className="min-w-0 flex-1 text-sm">
        <span className="font-medium">{step.label}</span>
        {step.detail && <span className="block truncate text-xs text-muted">{step.detail}</span>}
      </span>
      {step.folder === "others" && p.requested.length > 0 && (
        <button
          type="button"
          disabled={!onNudge}
          onClick={onNudge}
          title={`Mention ${p.requested.map((r) => `@${r.login}`).join(" ")} in a comment`}
          className={`${secondary} h-8 px-3`}
        >
          Nudge
        </button>
      )}
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
