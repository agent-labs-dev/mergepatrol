import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import { Link, useNavigate, useSearch } from "@tanstack/react-router";
import { type ReactNode, useEffect, useReducer, useRef, useState } from "react";
import { AgentReviewScreen, reviewedPr } from "./AgentReview";
import {
  closePull,
  closeUrl,
  dequeuePull,
  fetchLanes,
  fetchSession,
  fetchYou,
  nudgePull,
  queuePull,
  readyPull,
  reviewPull,
  reviewUrl,
  seenPull,
  snoozePull,
  unsnoozePull,
} from "./api";
import type { InboxPullView } from "./api/InboxPullView";
import type { ReviewPull } from "./api/ReviewPull";
import type { VerdictView } from "./api/VerdictView";
import { PullDetail } from "./PullDetail";
import { entryOf, QueueActivity, QueueGate } from "./Queue";
import { Countdown, toasts } from "./Toasts";
import { Avatar, RepoIcon, timeAgo } from "./ui";

/** Needs you, then the user's own PRs by whose move it is, then those merged lately, then
 * those snoozed. */
export const FOLDERS = ["needs", "turn", "queue", "others", "drafts", "merged", "snoozed"] as const;
export type Folder = (typeof FOLDERS)[number];
type Own = Exclude<Folder, "needs" | "merged" | "snoozed">;

const folderNames: Record<Folder, string> = {
  needs: "Needs you",
  turn: "Your turn",
  queue: "In the queue",
  others: "Waiting on others",
  drafts: "Drafts",
  merged: "Merged",
  snoozed: "Snoozed",
};

const keyOf = (p: InboxPullView) => `${p.repo}#${p.number}`;

/** A review waits this long before it goes to GitHub, which cannot take one back: Undo until
 * then. */
const HOLD_MS = 5000;
/** The queue takes a label change within seconds. Past this, stop showing the click. */
const PENDING_MS = 60_000;

/** A queue click the queue has not caught up with. */
type Click = { kind: "queueing" | "dequeueing"; at: number };

/** What waits out `HOLD_MS`: a review, or closing the PR. */
type Act = { kind: "review"; review: ReviewPull } | { kind: "close" };

/** An act waiting out `HOLD_MS`, or on its way to GitHub, and the folder it was done in. */
type Held = {
  p: InboxPullView;
  act: Act;
  folder: Folder;
  timer: number;
  /** Its toast, which turns into the outcome. */
  toast: string;
  sending?: true;
};

/** Three panes, as the Queue page: the folders, the PRs in one, the selected PR with the review
 * box or its author's moves under it. Every repo of every workspace, as webhooks keep them. */
export function You() {
  const { folder = "needs", pr, answer } = useSearch({ from: "/$account/you" });
  const navigate = useNavigate({ from: "/$account/you" });
  const me = useQuery({ queryKey: ["session"], queryFn: fetchSession }).data?.user?.login ?? "";
  // Read from MergeLoop's own database, not GitHub: cheap to read often.
  const you = useQuery({ queryKey: ["you"], queryFn: fetchYou, refetchInterval: 5000 });
  const client = useQueryClient();
  const [clicks, setClicks] = useState(new Map<string, Click>());
  const [drafts, setDrafts] = useState(new Map<string, string>());
  const [error, setError] = useState<string | null>(null);
  const last = useRef(0);
  const [now, setNow] = useState(Date.now());
  const held = useRef(new Map<string, Held>());
  const [, render] = useReducer((n: number) => n + 1, 0);
  // Ticks each click until the data has it.
  useEffect(() => {
    const t = setInterval(() => setNow(Date.now()), 5000);
    return () => clearInterval(t);
  }, []);
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

  /** Sends a held act now. Its PR stays out of its folder until the inbox has it. */
  const send = async (key: string, h: Held) => {
    if (h.sending) return;
    h.sending = true;
    clearTimeout(h.timer);
    toasts.update(h.toast, { type: "loading", description: "Sending…", data: {} });
    render();
    const { p, act } = h;
    try {
      if (act.kind === "review") await reviewPull(p.workspace, p.repo, p.number, act.review);
      else await closePull(p.workspace, p.repo, p.number);
      await client.refetchQueries({ queryKey: ["you"] });
      toasts.update(h.toast, {
        type: "success",
        title:
          act.kind === "review"
            ? `${verdicts[act.review.verdict]} ${shortRef(p)}${act.review.queue ? " and queued it" : ""}`
            : `Closed ${shortRef(p)}`,
        description: undefined,
        timeout: 4000,
      });
    } catch (e) {
      toasts.close(h.toast);
      setError((e as Error).message);
      if (act.kind === "review") setDraft(key, act.review.body);
    } finally {
      // A review held for the PR since is not this one's to drop.
      if (held.current.get(key) === h) held.current.delete(key);
      render();
    }
  };
  const hold = (p: InboxPullView, act: Act) => {
    const key = keyOf(p);
    // An act already waiting on the PR, a comment say, goes now.
    const waiting = held.current.get(key);
    if (waiting) send(key, waiting);
    const until = Date.now() + HOLD_MS;
    const h: Held = { p, act, folder, timer: 0, toast: "" };
    h.timer = window.setTimeout(() => send(key, h), HOLD_MS);
    h.toast = toasts.add({
      type: "held",
      title: `${act.kind === "review" ? holding[act.review.verdict] : "Closing"} ${shortRef(p)}`,
      description: <Countdown until={until} verb={act.kind === "review" ? "Sends" : "Closes"} />,
      timeout: 0,
      data: {
        ms: HOLD_MS,
        actions: [
          { label: "Send now", onClick: () => send(key, h) },
          { label: "Undo", onClick: () => undo(key), primary: true },
        ],
      },
    });
    held.current.set(key, h);
    if (act.kind === "review") setDraft(key, "");
    render();
  };
  const undo = (key: string) => {
    const h = held.current.get(key);
    if (!h || h.sending) return;
    clearTimeout(h.timer);
    toasts.close(h.toast);
    held.current.delete(key);
    if (h.act.kind === "review") setDraft(key, h.act.review.body);
    navigate({ search: { folder: h.folder, pr: key } });
    render();
  };
  // Leaving the page sends what waits: a closing tab, or another tab of the dashboard.
  useEffect(() => {
    const flush = () => {
      for (const [key, h] of held.current)
        if (!h.sending) {
          clearTimeout(h.timer);
          toasts.close(h.toast);
          held.current.delete(key);
          const { p, act } = h;
          fetch(
            act.kind === "review"
              ? reviewUrl(p.workspace, p.repo, p.number)
              : closeUrl(p.workspace, p.repo, p.number),
            {
              method: "POST",
              headers: { "content-type": "application/json" },
              body: act.kind === "review" ? JSON.stringify(act.review) : undefined,
              keepalive: true,
            },
          );
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
      toasts.add({
        type: "success",
        title: `${priority ? "Queued at the front:" : "Queued"} ${shortRef(p)}`,
      });
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
      toasts.add({ type: "success", title: `Took ${shortRef(p)} out of the queue` });
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
      toasts.add({
        type: "success",
        title: `Asked ${p.requested.map((r) => r.login).join(", ")} to review ${shortRef(p)}`,
      }),
    onError: (e) => setError(e.message),
  });

  const snooze = useMutation({
    mutationFn: async ({ p, minutes }: { p: InboxPullView; minutes: number }) => {
      await snoozePull(p.workspace, p.repo, p.number, minutes);
      await client.refetchQueries({ queryKey: ["you"] });
    },
    onSuccess: (_, { p, minutes }) =>
      toasts.add({
        type: "success",
        title: `Snoozed ${shortRef(p)} until ${untilText(Date.now() + minutes * MINUTE)}`,
      }),
    onError: (e) => setError(e.message),
  });
  const publish = useMutation({
    mutationFn: async (p: InboxPullView) => {
      await readyPull(p.workspace, p.repo, p.number);
      await client.refetchQueries({ queryKey: ["you"] });
    },
    onSuccess: (_, p) =>
      toasts.add({ type: "success", title: `Published ${shortRef(p)} for review` }),
    onError: (e) => setError(e.message),
  });
  const unsnooze = useMutation({
    mutationFn: async (p: InboxPullView) => {
      await unsnoozePull(p.workspace, p.repo, p.number);
      await client.refetchQueries({ queryKey: ["you"] });
    },
    onSuccess: (_, p) => toasts.add({ type: "success", title: `${shortRef(p)} is back` }),
    onError: (e) => setError(e.message),
  });

  const data = you.data;
  // An act waiting or on its way takes its PR out, but a comment, which answers nothing.
  const acting = (p: InboxPullView) => {
    const act = held.current.get(keyOf(p))?.act;
    return act !== undefined && !(act.kind === "review" && act.review.verdict === "comment");
  };
  const awake = (p: InboxPullView) => !p.snoozed_until && !acting(p);
  const needs = (data?.review_requested ?? [])
    .filter(awake)
    // The queue's first, then those back for another look; each updated last first.
    .sort((a, b) => reasonRank[reasonOf(a, me).kind] - reasonRank[reasonOf(b, me).kind]);
  const own = (data?.authored ?? []).filter(awake).map((p) => ({ p, step: stepOf(p, clickOf(p)) }));
  const lists: Record<Folder, InboxPullView[]> = {
    needs,
    // In its sections' order, so j and k walk them top to bottom.
    turn: own
      .filter((o) => o.step.folder === "turn")
      .sort((a, b) => turnRank(a.step) - turnRank(b.step))
      .map((o) => o.p),
    queue: own.filter((o) => o.step.folder === "queue").map((o) => o.p),
    others: own.filter((o) => o.step.folder === "others").map((o) => o.p),
    drafts: own.filter((o) => o.step.folder === "drafts").map((o) => o.p),
    merged: data?.merged ?? [],
    // The first back first.
    snoozed: [...(data?.review_requested ?? []), ...(data?.authored ?? [])]
      .filter((p) => p.snoozed_until && !acting(p))
      .sort((a, b) => (a.snoozed_until ?? "").localeCompare(b.snoozed_until ?? "")),
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

  // The queue's own view of a queued PR, shared with the Queue page: its hold and review.
  const lanes = useQuery({
    queryKey: ["lanes", pull?.workspace],
    queryFn: () => fetchLanes(pull?.workspace ?? 0),
    enabled: !!pull?.queue,
    refetchInterval: 3000,
  });
  const lane = pull?.queue
    ? lanes.data?.find((l) => l.repo === pull.repo && l.trunk === pull.base)
    : undefined;
  const entry = pull && lane && entryOf(lane, pull.number);

  const select = (p?: InboxPullView) =>
    navigate({ search: (s) => ({ ...s, pr: p && keyOf(p), answer: undefined }) });
  /** A PR the queue's events name: here when it is in a folder, else on GitHub. */
  const selectNamed = (repo: string, n: number) => {
    const f = FOLDERS.find((f) => lists[f].some((p) => p.repo === repo && p.number === n));
    if (f) navigate({ search: { folder: f, pr: `${repo}#${n}` } });
    else window.open(`https://github.com/${repo}/pull/${n}`, "_blank", "noreferrer");
  };
  const open = (f: Folder) => {
    last.current = 0;
    navigate({ search: { folder: f } });
  };
  const newest = [...held.current.keys()].at(-1);
  const [snoozing, setSnoozing] = useState(false);
  // biome-ignore lint/correctness/useExhaustiveDependencies: the menu is for the PR it opened on.
  useEffect(() => setSnoozing(false), [pullKey]);
  useKeys(
    answer
      ? {}
      : {
          j: () => select(list[Math.min(i + 1, list.length - 1)]),
          k: () => select(list[Math.max(i - 1, 0)]),
          a: () =>
            pull &&
            folder === "needs" &&
            hold(pull, {
              kind: "review",
              review: { verdict: "approve", body: drafts.get(keyOf(pull)) ?? "", queue: false },
            }),
          r: () => pull && folder === "needs" && document.getElementById("review-body")?.focus(),
          u: () => newest && undo(newest),
          q: () =>
            pull &&
            folder !== "needs" &&
            stepOf(pull, clickOf(pull)).ready &&
            queue.mutate({ p: pull, priority: false }),
          s: () => pull && folder !== "merged" && setSnoozing(true),
          ...Object.fromEntries(FOLDERS.map((f, n) => [String(n + 1), () => open(f)])),
        },
  );

  if (you.isError)
    return <p className="p-6 text-bad">Could not load your PRs: {you.error.message}</p>;
  if (answer && pull && lane && entry)
    return (
      <AgentReviewScreen
        workspace={pull.workspace}
        repo={pull.repo}
        pr={reviewedPr(entry)}
        title={pull.title}
        author={pull.author?.login}
        onExit={() => navigate({ search: (s) => ({ ...s, answer: undefined }) })}
      />
    );
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
            {f === "snoozed" && <div className="mt-4 max-lg:hidden" />}
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
          <dd>undo a review or close</dd>
          <dt>
            <Kbd>q</Kbd>
          </dt>
          <dd>queue yours</dd>
          <dt>
            <Kbd>s</Kbd>
          </dt>
          <dd>snooze</dd>
          <dt>
            <Kbd>1</Kbd>–<Kbd>7</Kbd>
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
                : folder === "snoozed"
                  ? "Out of the way until their time is up, or until someone else comments or reviews."
                  : folder === "turn"
                    ? "Your open PRs that wait on you, by what they need. Those the queue holds come first."
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
        <Groups
          sections={
            folder === "needs"
              ? reasonSections(list, me)
              : folder === "turn"
                ? turnSectionsOf(list, (p) => stepOf(p, clickOf(p)))
                : undefined
          }
          list={list}
        >
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
                ) : folder === "snoozed" ? (
                  <span className="truncate text-muted">
                    Snoozed until {p.snoozed_until && untilText(Date.parse(p.snoozed_until))}
                  </span>
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
            gate={
              lane &&
              entry && (
                <QueueGate
                  workspace={pull.workspace}
                  lane={lane}
                  entry={entry}
                  onReview={() => navigate({ search: (s) => ({ ...s, answer: true }) })}
                />
              )
            }
            activity={
              lane &&
              entry && (
                <QueueActivity
                  workspace={pull.workspace}
                  lane={lane}
                  entry={entry}
                  onSelect={(n) => selectNamed(pull.repo, n)}
                />
              )
            }
            onBack={() => select()}
            footer={(() => {
              const later = (
                <Later
                  open={snoozing}
                  onOpen={setSnoozing}
                  busy={snooze.isPending}
                  onSnooze={(minutes) => snooze.mutate({ p: pull, minutes })}
                  onClose={() => hold(pull, { kind: "close" })}
                />
              );
              if (folder === "needs")
                return (
                  <ReviewBox
                    pull={pull}
                    body={drafts.get(keyOf(pull)) ?? ""}
                    onBody={(text) => setDraft(keyOf(pull), text)}
                    onReview={(review) => hold(pull, { kind: "review", review })}
                    later={later}
                  />
                );
              if (folder === "merged") return null;
              if (folder === "snoozed")
                return (
                  <div className="flex flex-wrap items-center gap-2 border-t border-line bg-surface px-4 py-3 sm:px-6">
                    <span className="min-w-0 flex-1 text-sm">
                      <span className="font-medium">
                        Snoozed until{" "}
                        {pull.snoozed_until && untilText(Date.parse(pull.snoozed_until))}
                      </span>
                      <span className="block truncate text-xs text-muted">
                        Back sooner if someone else comments or reviews
                      </span>
                    </span>
                    {later}
                    <button
                      type="button"
                      disabled={unsnooze.isPending}
                      onClick={() => unsnooze.mutate(pull)}
                      className={solid}
                    >
                      Unsnooze
                    </button>
                  </div>
                );
              return (
                <OwnerBar
                  pull={pull}
                  step={stepOf(pull, clickOf(pull))}
                  busy={!!clickOf(pull)}
                  onQueue={(priority) => queue.mutate({ p: pull, priority })}
                  onDequeue={() => dequeue.mutate(pull)}
                  onNudge={nudge.isPending ? undefined : () => nudge.mutate(pull)}
                  onPublish={publish.isPending ? undefined : () => publish.mutate(pull)}
                  later={later}
                />
              );
            })()}
          />
        )}
      </section>
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

// ── Snoozing ─────────────────────────────────────────────────────────────────

const MINUTE = 60_000;
const HOUR = 60 * MINUTE;

/** When a snooze ends, as a person says it: a time today, a weekday this week, else a date. */
function untilText(ms: number) {
  const at = new Date(ms);
  const now = new Date();
  const hour = { hour: "numeric", minute: at.getMinutes() ? "2-digit" : undefined } as const;
  if (at.toDateString() === now.toDateString()) return at.toLocaleTimeString(undefined, hour);
  if (ms - now.getTime() < 6 * 24 * HOUR)
    return at.toLocaleString(undefined, { weekday: "short", ...hour });
  return at.toLocaleDateString(undefined, { month: "short", day: "numeric" });
}

/** At `hour` o'clock, `days` from today. */
function morning(days: number, hour = 9) {
  const at = new Date();
  at.setDate(at.getDate() + days);
  at.setHours(hour, 0, 0, 0);
  return at.getTime();
}

/** Where a snooze can end, and in how many minutes, as the API takes it: on the minute. */
function snoozeChoices() {
  const now = Date.now();
  const monday = (8 - new Date().getDay()) % 7 || 7;
  return [
    { label: "Later today", at: now + 3 * HOUR },
    { label: "Tomorrow", at: morning(1) },
    { label: "Next week", at: morning(monday) },
    { label: "In a month", at: morning(30) },
  ].map((c) => ({ ...c, minutes: Math.max(1, Math.ceil((c.at - now) / MINUTE)) }));
}

// ── Buttons ──────────────────────────────────────────────────────────────────

/** The footers' buttons, all one height. */
const button =
  "inline-flex h-8 shrink-0 items-center justify-center gap-1.5 rounded-md px-3 text-[13px] font-medium transition-colors disabled:opacity-40";
const outline = `${button} border border-line-strong bg-surface shadow-xs hover:bg-sunken`;
const plain = `${outline} text-fg-2 hover:text-fg`;
const solid = `${button} bg-fg text-surface hover:bg-fg-2`;
const iconButton =
  "inline-flex size-8 shrink-0 items-center justify-center rounded-md text-muted transition-colors hover:bg-sunken hover:text-fg disabled:opacity-40";

const icons = {
  // An alarm clock: its face, hands, and two bells.
  snooze: "M8 14a5 5 0 1 0 0-10a5 5 0 0 0 0 10M8 6.5V9l1.5 1M2.5 4l2-2M13.5 4l-2-2",
  // GitHub's closed PR, as the Queue page draws it.
  close:
    "M5.5 3.5a1.5 1.5 0 1 1-3 0a1.5 1.5 0 1 1 3 0M4 5v6M5.5 12.5a1.5 1.5 0 1 1-3 0a1.5 1.5 0 1 1 3 0M13.5 12.5a1.5 1.5 0 1 1-3 0a1.5 1.5 0 1 1 3 0M12 11V8M10.5 2.5l3 3M13.5 2.5l-3 3",
  caret: "M4.5 6.5 8 10l3.5-3.5",
};

function Icon({ d, className = "size-4" }: { d: string; className?: string }) {
  return (
    <svg
      viewBox="0 0 16 16"
      className={className}
      fill="none"
      stroke="currentColor"
      strokeWidth="1.5"
      strokeLinecap="round"
      strokeLinejoin="round"
      aria-hidden="true"
    >
      <path d={d} />
    </svg>
  );
}

/** A menu under `ref` that its first item takes focus in, and a click elsewhere or Escape
 * closes. */
function useMenu(open: boolean, onOpen: (open: boolean) => void) {
  const ref = useRef<HTMLSpanElement>(null);
  useEffect(() => {
    if (!open) return;
    ref.current?.querySelector<HTMLElement>("[role=menu] button")?.focus();
    const away = (e: MouseEvent) => {
      if (!ref.current?.contains(e.target as Node)) onOpen(false);
    };
    const esc = (e: KeyboardEvent) => e.key === "Escape" && onOpen(false);
    document.addEventListener("mousedown", away);
    document.addEventListener("keydown", esc);
    return () => {
      document.removeEventListener("mousedown", away);
      document.removeEventListener("keydown", esc);
    };
  }, [open, onOpen]);
  return ref;
}

/** Snooze, with its menu of when, and close: for a PR that is not ready, or not wanted. */
function Later({
  open,
  onOpen,
  busy,
  onSnooze,
  onClose,
}: {
  open: boolean;
  onOpen: (open: boolean) => void;
  busy: boolean;
  onSnooze: (minutes: number) => void;
  onClose: () => void;
}) {
  const ref = useMenu(open, onOpen);
  return (
    <>
      <span ref={ref} className="relative">
        <button
          type="button"
          disabled={busy}
          onClick={() => onOpen(!open)}
          title="Snooze (s)"
          aria-label="Snooze"
          className={`${iconButton} ${open ? "bg-sunken text-fg" : ""}`}
        >
          <Icon d={icons.snooze} />
        </button>
        {open && (
          <div
            role="menu"
            className="absolute bottom-10 left-0 z-30 w-48 overflow-hidden rounded-lg border border-line bg-surface py-1 text-sm shadow-lg"
          >
            {snoozeChoices().map((c) => (
              <button
                key={c.label}
                type="button"
                onClick={() => {
                  onOpen(false);
                  onSnooze(c.minutes);
                }}
                className="flex w-full items-center justify-between gap-3 px-3 py-1.5 text-left hover:bg-sunken focus:bg-sunken focus:outline-none"
              >
                {c.label}
                <span className="text-xs text-muted">{untilText(c.at)}</span>
              </button>
            ))}
          </div>
        )}
      </span>
      <button
        type="button"
        onClick={onClose}
        title="Close the PR on GitHub, as you"
        aria-label="Close the PR"
        className={`${iconButton} text-bad/70! hover:bg-red-50! hover:text-bad! dark:hover:bg-red-400/10!`}
      >
        <Icon d={icons.close} />
      </button>
    </>
  );
}

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

/** Needs you's PRs by why. */
const reasonSections = (list: InboxPullView[], me: string): Section[] =>
  (["held", "again", "asked"] as const).map((kind) => ({
    key: kind,
    label: reasonGroups[kind],
    ps: list.filter((p) => reasonOf(p, me).kind === kind),
  }));

type Section = { key: string; label: string; ps: InboxPullView[] };

/** A folder in sections, each with a header, an empty one left out. A folder without them as one
 * list. */
function Groups({
  sections,
  list,
  children,
}: {
  sections?: Section[];
  list: InboxPullView[];
  children: (p: InboxPullView) => ReactNode;
}) {
  if (!sections)
    return (
      <ol>
        {list.map((p) => (
          <li key={keyOf(p)}>{children(p)}</li>
        ))}
      </ol>
    );
  return sections.map(({ key, label, ps }) => {
    if (!ps.length) return null;
    return (
      <div key={key}>
        <p className="border-b border-line-soft bg-sunken px-4 py-1.5 text-xs font-medium text-muted">
          {label} · {ps.length}
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

/** Why a PR of yours is your turn: its section of Your turn. */
type Turn = "held" | "ready" | "changes" | "failing" | "off";

type Step = { label: string; detail?: string; ready?: boolean } & (
  | { folder: Exclude<Own, "turn"> }
  | { folder: "turn"; turn: Turn }
);

/** Your turn's sections, in order: what holds up the queue, what you can queue, what needs work,
 * then what MergeLoop can't queue. */
const turnSections: Record<Turn, string> = {
  held: "Held in the queue: needs your decision",
  ready: "Ready to queue",
  changes: "Changes requested",
  failing: "Checks failing",
  off: "Not queued: MergeLoop is off in the repo",
};
const turnOrder = Object.keys(turnSections) as Turn[];
const turnRank = (s: Step) => (s.folder === "turn" ? turnOrder.indexOf(s.turn) : turnOrder.length);

const turnSectionsOf = (list: InboxPullView[], step: (p: InboxPullView) => Step): Section[] =>
  turnOrder.map((turn) => ({
    key: turn,
    label: turnSections[turn],
    ps: list.filter((p) => {
      const s = step(p);
      return s.folder === "turn" && s.turn === turn;
    }),
  }));

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
    return {
      folder: "turn",
      turn: "held",
      label: "Held in the queue",
      detail: q.detail ?? undefined,
    };
  if (p.draft) return { folder: "drafts", label: "Draft" };
  if (p.review_decision === "changes_requested") {
    const who = p.reviews.filter((r) => !r.approved).map((r) => r.reviewer.login);
    return {
      folder: "turn",
      turn: "changes",
      label: "Changes requested",
      detail: who.length ? `by ${who.join(", ")}` : undefined,
    };
  }
  if (p.checks === "failed") return { folder: "turn", turn: "failing", label: "Checks failing" };
  if (p.review_decision === "review_required")
    return {
      folder: "others",
      label: "Waiting on a review",
      detail: p.requested.length ? `from ${p.requested.map((r) => r.login).join(", ")}` : undefined,
    };
  if (p.checks === "pending") return { folder: "others", label: "Checks running" };
  return p.queues
    ? { folder: "turn", turn: "ready", label: "Ready to queue", ready: true }
    : {
        folder: "turn",
        turn: "off",
        label: "Ready to merge",
        detail: "MergeLoop is not on in this repo",
      };
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

/** Approve, request changes or comment, as the user. ⌘↵ presses Approve. A change request or a comment
 * needs words. The words stay with the PR, kept by the page, until the review goes. */
function ReviewBox({
  pull: p,
  body,
  onBody,
  onReview,
  later,
}: {
  pull: InboxPullView;
  body: string;
  onBody: (body: string) => void;
  onReview: (review: ReviewPull) => void;
  later: ReactNode;
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
  const [choice, setChoice] = useApproveChoice();
  return (
    <div className="border-t border-line bg-surface px-4 py-3 sm:px-6">
      {p.queue?.state === "held" && (
        <p className="mb-2 text-xs text-muted">The queue holds it: your approval may let it go.</p>
      )}
      <div
        className={`rounded-lg border bg-page shadow-xs focus-within:ring-2 focus-within:ring-fg/10 ${
          needsWords ? "border-bad" : "border-line-strong focus-within:border-fg/40"
        }`}
      >
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
              send("approve", canQueue && choice === "queue");
            }
            if (e.key === "Escape") e.currentTarget.blur();
          }}
          rows={2}
          placeholder="Leave a review comment…"
          className="block w-full resize-none bg-transparent px-3 pt-2.5 pb-1 text-sm outline-none placeholder:text-faint"
        />
        <div className="flex flex-wrap items-center gap-1 px-1.5 pb-1.5">
          {later}
          <span className="ml-auto flex items-center gap-1.5">
            <button type="button" onClick={() => send("comment")} className={plain}>
              Comment
            </button>
            <button
              type="button"
              onClick={() => send("request_changes")}
              className={`${outline} text-bad`}
            >
              Request changes
            </button>
            <ApproveButton
              canQueue={canQueue}
              choice={choice}
              onChoice={setChoice}
              onApprove={(queue) => send("approve", queue)}
            />
          </span>
        </div>
      </div>
      {needsWords && (
        <p className="mt-1.5 text-xs text-bad">Say what to change, or what you think.</p>
      )}
    </div>
  );
}

type ApproveChoice = "approve" | "queue";

/** What the Approve button does, as the user last picked it, on every PR it can do it on. */
function useApproveChoice() {
  const [choice, set] = useState<ApproveChoice>(() =>
    localStorage.getItem("approve") === "queue" ? "queue" : "approve",
  );
  const pick = (c: ApproveChoice) => {
    localStorage.setItem("approve", c);
    set(c);
  };
  return [choice, pick] as const;
}

const approveChoices: Record<ApproveChoice, { label: string; detail: string }> = {
  approve: { label: "Approve", detail: "Leave it to the author to queue." },
  queue: { label: "Approve & queue", detail: "Then queue it to merge." },
};

/** Approve, or approve and queue, as GitHub's merge button picks how to merge: a pick from the
 * menu does it, and the button does the last one picked from then on, on every PR. A PR that
 * cannot queue only approves. ⌘↵ presses the button. */
function ApproveButton({
  canQueue,
  choice,
  onChoice,
  onApprove,
}: {
  canQueue: boolean;
  choice: ApproveChoice;
  onChoice: (c: ApproveChoice) => void;
  onApprove: (queue: boolean) => void;
}) {
  const [open, setOpen] = useState(false);
  const ref = useMenu(open, setOpen);
  const now = canQueue ? choice : "approve";
  const green = `${button} bg-emerald-600 text-white hover:bg-emerald-700`;
  return (
    <span ref={ref} className="relative flex">
      <button
        type="button"
        onClick={() => onApprove(now === "queue")}
        title={`${approveChoices[now].label} (⌘↵)`}
        className={`${green} ${canQueue ? "rounded-r-none" : ""}`}
      >
        {approveChoices[now].label}
      </button>
      {canQueue && (
        <button
          type="button"
          onClick={() => setOpen(!open)}
          aria-label="Choose how to approve"
          className={`${green} rounded-l-none border-l border-white/25 px-1.5`}
        >
          <Icon d={icons.caret} className="size-3.5" />
        </button>
      )}
      {open && (
        <div
          role="menu"
          className="absolute right-0 bottom-10 z-30 w-72 overflow-hidden rounded-lg border border-line bg-surface py-1 text-sm shadow-lg"
        >
          {(["approve", "queue"] as const).map((c) => (
            <button
              key={c}
              type="button"
              onClick={() => {
                onChoice(c);
                setOpen(false);
                onApprove(c === "queue");
              }}
              className="flex w-full items-start gap-2 px-3 py-2 text-left hover:bg-sunken focus:bg-sunken focus:outline-none"
            >
              <span className="w-4 shrink-0 text-ok">{c === choice ? "✓" : ""}</span>
              <span>
                <span className="block font-medium">{approveChoices[c].label}</span>
                <span className="block text-xs text-muted">{approveChoices[c].detail}</span>
              </span>
            </button>
          ))}
        </div>
      )}
    </span>
  );
}

/** The author's moves on their own PR: queue it, or take it out, snooze or close it. */
function OwnerBar({
  pull: p,
  step,
  busy,
  onQueue,
  onDequeue,
  onNudge,
  onPublish,
  later,
}: {
  pull: InboxPullView;
  step: Step;
  busy: boolean;
  onQueue: (priority: boolean) => void;
  onDequeue: () => void;
  /** Mentions the reviewers who have not answered. `undefined` while it goes. */
  onNudge?: () => void;
  /** Marks a draft ready for review. `undefined` while it goes. */
  onPublish?: () => void;
  later: ReactNode;
}) {
  return (
    <div className="flex flex-wrap items-center gap-2 border-t border-line bg-surface px-4 py-3 sm:px-6">
      <span className="min-w-0 flex-1 text-sm">
        <span className="font-medium">{step.label}</span>
        {step.detail && <span className="block truncate text-xs text-muted">{step.detail}</span>}
      </span>
      {later}
      {step.folder === "others" && p.requested.length > 0 && (
        <button
          type="button"
          disabled={!onNudge}
          onClick={onNudge}
          title={`Mention ${p.requested.map((r) => `@${r.login}`).join(" ")} in a comment`}
          className={plain}
        >
          Nudge
        </button>
      )}
      {p.draft && (
        <button
          type="button"
          disabled={!onPublish}
          onClick={onPublish}
          title="Mark it ready for review on GitHub, as you"
          className={solid}
        >
          Publish
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
              className={`${outline} text-hot`}
            >
              ↑ Priority
            </button>
          )}
          <button type="button" disabled={busy} onClick={onDequeue} className={plain}>
            Dequeue
          </button>
        </>
      ) : !p.queues ? (
        <Link
          to="/$account/repos"
          params={{ account: p.repo.split("/")[0] }}
          search={{ repo: p.repo }}
          className={solid}
        >
          Turn on MergeLoop
        </Link>
      ) : (
        !p.draft && (
          <>
            <button
              type="button"
              disabled={busy}
              onClick={() => onQueue(true)}
              title="Queue at the front"
              className={`${outline} text-hot`}
            >
              ↑ Priority
            </button>
            <button
              type="button"
              disabled={busy}
              onClick={() => onQueue(false)}
              className={step.ready ? solid : plain}
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
