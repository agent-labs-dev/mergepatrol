import { useMutation, useMutationState, useQuery, useQueryClient } from "@tanstack/react-query";
import { useNavigate, useSearch } from "@tanstack/react-router";
import { useEffect, useState } from "react";
import { Activity } from "./Activity";
import { AgentReviewScreen, Gate, reviewBadge, reviewedPr, reviewLine } from "./AgentReview";
import {
  closePull,
  dequeuePull,
  fetchEvents,
  fetchLanes,
  fetchOpenPulls,
  fetchSession,
  queuePull,
} from "./api";
import type { ChecksView } from "./api/ChecksView";
import type { EntryView } from "./api/EntryView";
import type { LaneView } from "./api/LaneView";
import type { OpenPullView } from "./api/OpenPullView";
import { matches, narrows, type OpenFilter, OpenFilters } from "./OpenFilters";
import { PullDetail } from "./PullDetail";
import { Avatar, primary, secondary } from "./ui";

const laneKey = (l: LaneView) => `${l.repo}:${l.trunk}`;

/** Three panes: the lanes, the selected lane's PRs in merge order, the selected PR. A narrow
 * screen shows the lanes and the PRs until a PR is picked, then the PR alone. */
export function Queue({ workspace }: { workspace: number }) {
  const { lane: laneId, pr, answer, ...filter } = useSearch({ from: "/$account/queue" });
  const navigate = useNavigate({ from: "/$account/queue" });
  const setPr = (pr?: number) => navigate({ search: (s) => ({ ...s, pr, answer: undefined }) });
  const setAnswer = (answer?: boolean) => navigate({ search: (s) => ({ ...s, answer }) });
  // Replaced, not pushed: each keystroke of the search changes it.
  const setFilter = (f: OpenFilter) =>
    navigate({
      search: (s) => ({ lane: s.lane, pr: s.pr, answer: s.answer, ...f }),
      replace: true,
    });
  const me = useQuery({ queryKey: ["session"], queryFn: fetchSession }).data?.user?.login;
  const lanes = useQuery({
    queryKey: ["lanes", workspace],
    queryFn: () => fetchLanes(workspace),
    refetchInterval: 3000,
  });
  const current = lanes.data?.find((l) => laneKey(l) === laneId) ?? lanes.data?.[0];
  const open = useQuery({
    queryKey: ["open", workspace, current?.repo],
    queryFn: () => fetchOpenPulls(workspace, current?.repo ?? ""),
    enabled: current !== undefined,
    // Read as the user, on their own rate limit. A label change shows sooner through the lane.
    refetchInterval: 30_000,
  });
  const actions = useActions(workspace, current, open.data, open.dataUpdatedAt);

  if (lanes.isError) return <p className="p-6 text-bad">Could not load the queue.</p>;
  if (lanes.data?.length === 0)
    return (
      <div className="grid h-full place-items-center text-center">
        <div>
          <p className="font-medium">No queues yet</p>
          <p className="mt-1 max-w-xs text-muted">
            Set up a repo in Repos, then add the{" "}
            <code className="rounded bg-sunken px-1 text-xs">mergepatrol</code> label to a PR.
          </p>
        </div>
      </div>
    );
  if (!lanes.data) return null;

  const lane = current ?? lanes.data[0];
  const { pending } = actions;
  const queued = withClicks(lane, pending);
  // Removed: at the top of the open PRs, as the PR GitHub updated last.
  const leaving = [...pending.values()].flatMap((p) =>
    p.kind === "removing" ? [{ ...p.item, state: "open" as const, priority: false }] : [],
  );
  // What the filters narrow: the open PRs, without any the lane or `leaving` has already.
  const notQueued = (open.data ?? []).filter(
    (p) =>
      ![...queued, ...leaving].some((e) => e.number === p.number || e.layers.includes(p.number)),
  );
  const unqueued = [...leaving, ...notQueued.filter(matches(filter, me)).map(asItem)];
  const items: Item[] = [...queued, ...unqueued];
  // A link to a stack's layer, as the agent's review gives, picks its stack.
  const entry =
    items.find((e) => e.number === pr) ??
    items.find((e) => pr && e.layers.includes(pr)) ??
    items[0];
  // The queue lists the active PRs first, so the batch is the head of the lane.
  const batched = lane.batch === null ? [] : queued.filter((e) => e.state === "active");
  const rest = queued.slice(batched.length);
  const picked =
    entry !== undefined && (entry.number === pr || (!!pr && entry.layers.includes(pr)));
  if (answer && picked && entry.state !== "open")
    return (
      <AgentReviewScreen
        workspace={workspace}
        repo={lane.repo}
        pr={reviewedPr({ ...entry, state: entry.state })}
        title={titleOf(entry)}
        author={entry.author?.login}
        onExit={() => setAnswer(undefined)}
      />
    );
  const row = (e: Item) => (
    <div
      className={`group flex items-stretch border-b border-line-soft hover:bg-page ${
        e === entry ? "lg:bg-page" : ""
      }`}
    >
      <Row
        entry={e}
        place={
          e.state === "held" || e.state === "open"
            ? null
            : queued.findIndex((q) => q.number === e.number) + 1
        }
        selected={e === entry}
        pending={actions.kinds.get(e.number)}
        onClick={() => setPr(e.number)}
      />
      <RowActions item={e} actions={actions} busy={actions.kinds.has(e.number)} />
    </div>
  );
  return (
    <div className="flex h-full flex-col lg:grid lg:grid-cols-[220px_minmax(360px,1fr)_minmax(400px,1.1fr)]">
      <div
        className={`shrink-0 border-line p-2 max-lg:flex max-lg:gap-1 max-lg:overflow-x-auto max-lg:border-b lg:overflow-y-auto lg:border-r ${picked ? "max-lg:hidden" : ""}`}
      >
        <p className="px-2 py-2 text-xs font-medium text-muted max-lg:hidden">Queues</p>
        {lanes.data.map((l) => (
          <button
            key={laneKey(l)}
            type="button"
            // The filters stay: the same author's PRs, say, in each repo.
            onClick={() => navigate({ search: (s) => ({ ...s, lane: laneKey(l), pr: undefined }) })}
            className={`flex w-full items-center justify-between gap-3 rounded-md px-2 py-1.5 text-left max-lg:w-auto max-lg:shrink-0 ${
              l === lane ? "bg-surface shadow-xs ring-1 ring-line" : "hover:bg-sunken"
            }`}
          >
            <span className="truncate" title={l.repo}>
              {l.repo.split("/")[1]}
              <span className="text-faint"> → {l.trunk}</span>
            </span>
            <span className="flex items-center gap-1.5 text-xs text-faint tabular-nums">
              {l.entries.some((e) => e.state === "held") && (
                <span className="size-1.5 rounded-full bg-warn" title="A PR is held" />
              )}
              {l.entries.length}
            </span>
          </button>
        ))}
      </div>
      <div
        className={`min-h-0 flex-1 overflow-y-auto border-line bg-surface lg:border-r ${picked ? "max-lg:hidden" : ""}`}
      >
        <div className="sticky top-0 border-b border-line bg-surface px-4 py-3">
          <p className="font-medium">{lane.repo}</p>
          <p className="text-xs text-muted">Merge order into {lane.trunk}</p>
        </div>
        {actions.error && (
          <p className="flex items-start justify-between gap-3 border-b border-red-200 bg-red-50 px-4 py-2 text-red-800 dark:border-red-400/30 dark:bg-red-400/10 dark:text-red-200">
            {actions.error}
            <button
              type="button"
              onClick={actions.dismiss}
              className="shrink-0 font-medium hover:opacity-70"
            >
              Dismiss
            </button>
          </p>
        )}
        {queued.length === 0 && <p className="p-4 text-faint">Nothing queued</p>}
        <ol>
          {lane.batch !== null && (
            <li className="border-b border-line-soft">
              <p className="flex items-center gap-2 bg-sunken px-4 py-2 text-xs text-muted">
                <span className="font-medium text-fg-2">Batch</span>
                testing {batched.length} PRs together in
                <a
                  href={`https://github.com/${lane.repo}/pull/${lane.batch}`}
                  target="_blank"
                  rel="noreferrer"
                  className="tabular-nums text-fg-2 underline decoration-line-strong underline-offset-2 hover:text-fg hover:decoration-fg"
                >
                  #{lane.batch}
                </a>
              </p>
              <ol className="shadow-[inset_2px_0_0_0_var(--color-blue-400)]">
                {batched.map((e) => (
                  <li key={e.number}>{row(e)}</li>
                ))}
              </ol>
            </li>
          )}
          {rest.map((e) => (
            <li key={e.number}>{row(e)}</li>
          ))}
        </ol>
        {open.isError && (
          <p className="mt-6 border-y border-line bg-sunken px-4 py-2 text-xs text-bad">
            Could not load the open PRs: {open.error.message}
          </p>
        )}
        {(notQueued.length > 0 || unqueued.length > 0) && (
          <>
            <p className="mt-6 flex items-center justify-between border-y border-line bg-sunken px-4 py-2 text-xs text-muted">
              <span>
                <span className="font-medium text-fg-2">Open</span> · not queued
              </span>
              <span className="tabular-nums">
                {narrows(filter)
                  ? `${unqueued.length} of ${leaving.length + notQueued.length}`
                  : unqueued.length}
              </span>
            </p>
            <OpenFilters pulls={notQueued} me={me} filter={filter} onChange={setFilter} />
            {unqueued.length === 0 && (
              <p className="px-4 py-6 text-center text-faint">
                {narrows(filter) ? "No open PRs match." : "Only drafts are open."}{" "}
                <button
                  type="button"
                  onClick={() =>
                    setFilter(narrows(filter) ? { drafts: filter.drafts } : { drafts: true })
                  }
                  className="text-muted underline underline-offset-2 hover:text-fg"
                >
                  {narrows(filter) ? "Clear the filters" : "Show them"}
                </button>
              </p>
            )}
            <ol>
              {unqueued.map((e) => (
                <li key={e.number}>{row(e)}</li>
              ))}
            </ol>
          </>
        )}
      </div>
      <div className={`min-h-0 flex-1 overflow-y-auto ${picked ? "" : "max-lg:hidden"}`}>
        {entry && (
          <PullDetail
            key={entry.number}
            workspace={workspace}
            repo={lane.repo}
            trunk={lane.trunk}
            pull={
              entry.state === "open"
                ? {
                    ...entry,
                    draft: open.data?.find((o) => o.number === entry.number)?.draft ?? false,
                  }
                : queuedPull(lane.repo, { ...entry, state: entry.state })
            }
            gate={
              entry.state !== "open" && (
                <QueueGate
                  workspace={workspace}
                  lane={lane}
                  entry={{ ...entry, state: entry.state }}
                  onReview={() => setAnswer(true)}
                />
              )
            }
            activity={
              entry.state !== "open" && (
                <QueueActivity
                  workspace={workspace}
                  lane={lane}
                  entry={{ ...entry, state: entry.state }}
                  onSelect={setPr}
                />
              )
            }
            actions={
              entry.state === "open" && (
                <>
                  <button
                    type="button"
                    disabled={pending.has(entry.number)}
                    onClick={() => actions.queue(entry, false)}
                    className={`${primary} h-8 px-3`}
                  >
                    Queue
                  </button>
                  <button
                    type="button"
                    disabled={pending.has(entry.number)}
                    onClick={() => actions.queue(entry, true)}
                    title="Queue at the front"
                    className={`${secondary} h-8 px-3 text-hot!`}
                  >
                    ↑ Priority
                  </button>
                </>
              )
            }
            onBack={() => setPr()}
          />
        )}
      </div>
    </div>
  );
}

/** A PR in the list: queued, or open and not queued. An open one has no priority or place. */
type Item = Omit<EntryView, "state"> & { state: EntryView["state"] | "open" };

/** An open PR as the list shows it: `detail` says what the queue would do with it. */
const asItem = (p: OpenPullView): Item => ({
  ...p,
  priority: false,
  state: "open",
  layers: [],
  hold: null,
  review: null,
  detail: p.draft
    ? "A draft: the queue holds it until it is ready."
    : p.review_decision === "changes_requested"
      ? "Changes requested: the queue holds it until they are approved."
      : null,
});

/** A click the queue has not caught up with: GitHub has the labels, the lane or the open PRs
 * not yet. `left`: when the lane no longer had a removed PR, for the open PRs to read again. */
type Pending = { kind: "queueing" | "removing"; item: Item; repo: string; left?: number };
/** What a row shows as in flight: a queue click, or an open PR closing. */
type PendingKind = Pending["kind"] | "prioritizing" | "closing";

/** The queue looks at a repo within seconds of a label change. Past this, stop waiting. */
const PENDING_MS = 60_000;

const pendingKey = (repo: string, n: number) => `${repo}#${n}`;

/** Whether the open PRs have `item`, or a layer of its stack. */
const inOpen = (open: OpenPullView[] | undefined, item: Item) =>
  !!open?.some((o) => o.number === item.number || item.layers.includes(o.number));

/** Where the queue takes a PR from, as the server orders the lane. */
const rank = (e: Item) =>
  e.state === "active" ? 0 : e.state === "waiting" ? (e.priority ? 1 : 2) : 3;

/** The lane as it is once the queue has the clicks: queued PRs in, removed ones out, each in
 * merge order. A queued PR goes last of its rank, as the queue puts it. */
function withClicks(lane: LaneView, pending: ReadonlyMap<number, Pending>): Item[] {
  const entries: Item[] = lane.entries.flatMap((e) => {
    const p = pending.get(e.number);
    if (!p) return [e];
    return p.kind === "removing" ? [] : [{ ...e, priority: p.item.priority }];
  });
  for (const p of pending.values())
    if (p.kind === "queueing" && !entryOf(lane, p.item.number))
      entries.push({ ...p.item, state: "waiting", detail: null });
  // Stable: each keeps its place within its rank.
  return entries.sort((a, b) => rank(a) - rank(b));
}

function useActions(
  workspace: number,
  lane: LaneView | undefined,
  open: OpenPullView[] | undefined,
  /** When `open` was read. */
  openAt: number,
) {
  const client = useQueryClient();
  const [pending, setPending] = useState(new Map<string, Pending & { at: number }>());
  const [error, setError] = useState<string | null>(null);
  const drop = (key: string) =>
    setPending((p) => {
      const next = new Map(p);
      next.delete(key);
      return next;
    });
  // Each poll of the lane may show what a click did. A removed PR then waits for the open PRs,
  // read again once after. A poll that changes nothing keeps the same data, so the expiry needs its
  // own timer.
  useEffect(() => {
    const prune = () =>
      setPending((p) => {
        let changed = false;
        let readOpen = false;
        const next = new Map<string, Pending & { at: number }>();
        for (const [key, click] of p) {
          const { kind, item, repo, at } = click;
          const here = lane?.repo === repo;
          const e = here ? entryOf(lane, item.number) : undefined;
          const done =
            here &&
            (kind === "removing"
              ? !e && (click.left ?? Infinity) < openAt && inOpen(open, item)
              : e?.priority === item.priority);
          if (done || Date.now() - at >= PENDING_MS) {
            changed = true;
            continue;
          }
          if (kind === "removing" && here && !e && click.left === undefined) {
            changed = readOpen = true;
            next.set(key, { ...click, left: Date.now() });
          } else next.set(key, click);
        }
        if (readOpen) client.invalidateQueries({ queryKey: ["open"] });
        return changed ? next : p;
      });
    prune();
    const timer = setInterval(prune, 5_000);
    return () => clearInterval(timer);
  }, [lane, open, openAt, client]);
  const mutation = useMutation({
    mutationFn: ({ kind, item, repo }: Pending) =>
      kind === "removing"
        ? dequeuePull(workspace, repo, prOf(item))
        : queuePull(workspace, repo, prOf(item), item.priority),
    onMutate: (p) => {
      setError(null);
      setPending((m) =>
        new Map(m).set(pendingKey(p.repo, p.item.number), { ...p, at: Date.now() }),
      );
    },
    onError: (e, p) => {
      setError(e.message);
      drop(pendingKey(p.repo, p.item.number));
    },
    // The open PRs stay as they are, the clicked one in its place, until the lane has it.
    onSuccess: () => client.invalidateQueries({ queryKey: ["lanes"] }),
  });
  // GitHub closes it at once: it is closing until the open PRs are read again without it.
  const closing = useMutation({
    mutationKey: ["close", workspace],
    mutationFn: ({ item, repo }: Omit<Pending, "kind">) => closePull(workspace, repo, item.number),
    onMutate: () => setError(null),
    onError: (e) => setError(e.message),
    onSuccess: () => client.invalidateQueries({ queryKey: ["open"] }),
  });
  const repo = lane?.repo ?? "";
  const kinds = new Map<number, PendingKind>(
    [...pending.values()]
      .filter((p) => p.repo === repo)
      .map((p) => [
        p.item.number,
        // The item as clicked: an open PR is queueing, a queued one changing its priority.
        p.kind === "queueing" && p.item.state !== "open" ? "prioritizing" : p.kind,
      ]),
  );
  // Each close still running, not only the last one clicked.
  const closingNow = useMutationState({
    filters: { mutationKey: ["close", workspace], status: "pending" },
    select: (m) => m.state.variables as Omit<Pending, "kind">,
  });
  for (const c of closingNow) if (c.repo === repo) kinds.set(c.item.number, "closing");
  return {
    /** The lane's clicks still to show, by PR. */
    pending: new Map(
      [...pending.values()].filter((p) => p.repo === repo).map((p) => [p.item.number, p]),
    ) as ReadonlyMap<number, Pending>,
    /** What each of the lane's PRs has in flight. */
    kinds: kinds as ReadonlyMap<number, PendingKind>,
    error,
    dismiss: () => setError(null),
    queue: (item: Item, priority: boolean) =>
      mutation.mutate({ kind: "queueing", item: { ...item, priority }, repo }),
    remove: (item: Item) => mutation.mutate({ kind: "removing", item, repo }),
    close: (item: Item) => {
      if (window.confirm(`Close #${item.number} on GitHub?`)) closing.mutate({ item, repo });
    },
  };
}
type Actions = ReturnType<typeof useActions>;

const icons = {
  // Two chevrons up: to the front of the queue.
  priority: "M4 8.5 8 4.5l4 4M4 12.5l4-4 4 4",
  queue: "M8 3.5v9M3.5 8h9",
  remove: "M4.5 4.5l7 7M11.5 4.5l-7 7",
  // GitHub's closed PR: the base branch, the head, and a cross where it would merge.
  close:
    "M5.5 3.5a1.5 1.5 0 1 1-3 0a1.5 1.5 0 1 1 3 0M4 5v6M5.5 12.5a1.5 1.5 0 1 1-3 0a1.5 1.5 0 1 1 3 0M13.5 12.5a1.5 1.5 0 1 1-3 0a1.5 1.5 0 1 1 3 0M12 11V8M10.5 2.5l3 3M13.5 2.5l-3 3",
};

function IconButton({
  icon,
  label,
  onClick,
  disabled,
  className,
}: {
  icon: keyof typeof icons;
  label: string;
  onClick: () => void;
  disabled: boolean;
  className: string;
}) {
  return (
    <button
      type="button"
      onClick={onClick}
      disabled={disabled}
      title={label}
      aria-label={label}
      className={`grid size-7 place-items-center rounded-md hover:bg-sunken disabled:opacity-40 ${className}`}
    >
      <svg
        viewBox="0 0 16 16"
        className="size-4"
        fill="none"
        stroke="currentColor"
        strokeWidth="1.75"
        strokeLinecap="round"
        strokeLinejoin="round"
        aria-hidden
      >
        <path d={icons[icon]} />
      </svg>
    </button>
  );
}

/** Three slots at the end of every row, so every row lines up: close for an open PR, priority,
 * then queue or remove. A queued PR's are faint until hovered, but for a priority it has. */
function RowActions({ item, actions, busy }: { item: Item; actions: Actions; busy: boolean }) {
  const quiet = "text-faint/60 group-hover:text-muted group-focus-within:text-muted";
  const open = item.state === "open";
  return (
    <span className="flex shrink-0 items-center gap-0.5 pr-3">
      {open ? (
        <IconButton
          icon="close"
          label="Close pull request"
          disabled={busy}
          onClick={() => actions.close(item)}
          className={`${quiet} hover:text-bad!`}
        />
      ) : (
        <span className="size-7" />
      )}
      <IconButton
        icon="priority"
        label={open ? "Priority queue" : item.priority ? "Drop priority" : "Make priority"}
        disabled={busy}
        onClick={() => actions.queue(item, !item.priority)}
        className={
          item.priority
            ? "text-hot"
            : open
              ? "text-muted hover:text-hot"
              : `${quiet} hover:text-hot!`
        }
      />
      {open ? (
        <IconButton
          icon="queue"
          label="Queue"
          disabled={busy}
          onClick={() => actions.queue(item, false)}
          className="text-muted hover:text-fg"
        />
      ) : (
        <IconButton
          icon="remove"
          label="Remove from queue"
          disabled={busy}
          onClick={() => actions.remove(item)}
          className={`${quiet} hover:text-bad!`}
        />
      )}
    </span>
  );
}

const stateBadge: Record<Item["state"], [string, string]> = {
  open: ["Not queued", "bg-surface text-muted ring-line-strong"],
  active: [
    "Testing",
    "bg-blue-50 text-blue-700 ring-blue-600/20 dark:bg-blue-400/10 dark:text-blue-300 dark:ring-blue-400/30",
  ],
  waiting: ["Waiting", "bg-sunken text-fg-2 ring-neutral-500/20"],
  held: [
    "Held",
    "bg-amber-50 text-amber-800 ring-amber-600/20 dark:bg-amber-400/10 dark:text-amber-300 dark:ring-amber-400/30",
  ],
};

function State({ entry }: { entry: Item }) {
  const [label, style] =
    (entry.state === "open" ? null : reviewBadge({ ...entry, state: entry.state })) ??
    stateBadge[entry.state];
  return (
    <span
      className={`whitespace-nowrap rounded-md px-1.5 py-0.5 text-xs font-medium ring-1 ring-inset ${style}`}
    >
      {label}
    </span>
  );
}

/** A PR saved before the queue kept its details has no title until the next tick. */
const titleOf = (e: Item) => e.title || `Pull request #${prOf(e)}`;
/** A PR's own number, or a stack's top layer: the queue records a stack's events on each layer. */
const prOf = (e: Item) => e.layers.at(-1) ?? e.number;
const refsOf = (e: Item) =>
  e.layers.length ? `Stack ${e.layers.map((n) => `#${n}`).join(" ")}` : `#${e.number}`;

function Row({
  entry: e,
  place,
  selected,
  pending,
  onClick,
}: {
  entry: Item;
  /** `null` for a held or open PR: it has no place until it is released or queued. */
  place: number | null;
  selected: boolean;
  pending?: PendingKind;
  onClick: () => void;
}) {
  return (
    <button
      type="button"
      onClick={onClick}
      // A narrow screen lists the PRs only while none is open, so none shows as selected there.
      className={`flex min-w-0 flex-1 gap-3 px-4 py-3 text-left ${
        selected ? "lg:bg-page lg:shadow-[inset_2px_0_0_0_var(--color-fg)]" : ""
      }`}
    >
      <span className="w-4 pt-px text-right text-xs text-faint tabular-nums">{place}</span>
      <span className="min-w-0 flex-1">
        <span className="flex items-start justify-between gap-3">
          <span className="truncate font-medium">{titleOf(e)}</span>
          <State entry={e} />
        </span>
        <span className="mt-1 flex flex-wrap items-center gap-x-2 gap-y-1 text-xs text-muted">
          <span className="tabular-nums">{refsOf(e)}</span>
          {e.author && (
            <span className="flex items-center gap-1">
              <Avatar src={e.author.avatar_url} alt={e.author.login} size="size-4" />
              {e.author.login}
            </span>
          )}
          {e.priority && <span className="font-medium text-hot">↑ Priority</span>}
          {e.fork && <span title="From a fork">Fork</span>}
          <span className="ml-auto flex items-center gap-2">
            <Checks checks={e.checks} />
            <Diff entry={e} />
          </span>
        </span>
        {pending ? (
          <span className="mt-1 block text-muted">
            {pending === "closing"
              ? "Closing…"
              : pending === "removing"
                ? "Leaving the queue…"
                : pending === "queueing"
                  ? "Queueing…"
                  : "Changing its priority…"}
          </span>
        ) : e.state !== "open" && reviewLine({ ...e, state: e.state }) ? (
          <span className="mt-1 block text-xs">{reviewLine({ ...e, state: e.state })}</span>
        ) : (
          e.detail && <span className="mt-1 block text-warn">{e.detail}</span>
        )}
      </span>
    </button>
  );
}

const checksStyle: Record<ChecksView, [string, string, string]> = {
  passed: ["✓", "text-ok", "Checks passed"],
  failed: ["✕", "text-bad", "Checks failed"],
  pending: ["●", "text-warn", "Checks running"],
};

function Checks({ checks, label }: { checks: ChecksView | null; label?: boolean }) {
  if (!checks) return label ? <span className="text-faint">No checks</span> : null;
  const [icon, color, text] = checksStyle[checks];
  return (
    <span className={color} title={text}>
      {icon}
      {label && <span className="ml-1.5 text-fg">{text}</span>}
    </span>
  );
}

function Diff({ entry: e }: { entry: Item }) {
  if (!e.url) return null;
  return (
    <span className="font-mono tabular-nums">
      <span className="text-ok">+{e.additions}</span>{" "}
      <span className="text-bad">−{e.deletions}</span>
    </span>
  );
}

/** A queued PR's place in the queue, over its summary: why it is held, or the agent's review
 * and what it asks a human. The Queue and You pages show the same. */
export function QueueGate({
  workspace,
  lane,
  entry: e,
  onReview,
}: {
  workspace: number;
  lane: LaneView;
  entry: EntryView;
  /** Opens the screen that answers the agent's review. */
  onReview: () => void;
}) {
  return (
    <div className="mb-6">
      <p className="flex items-center gap-2 text-sm">
        <span className="text-muted">In the queue</span>
        <State entry={e} />
        {e.priority && <span className="text-xs font-medium text-hot">↑ Priority</span>}
        {e.fork && <span className="text-xs text-muted">From a fork</span>}
        {e.layers.length > 0 && <span className="text-xs text-muted">{refsOf(e)}</span>}
      </p>
      {e.state === "held" && !reviewBadge(e) && (
        <div className="mt-4 rounded-lg border border-amber-200 bg-amber-50 px-4 py-3 text-amber-900 dark:border-amber-400/30 dark:bg-amber-400/10 dark:text-amber-200">
          <p className="font-medium">Held</p>
          <p className="mt-0.5">{e.detail}</p>
        </div>
      )}
      {e.review && <Gate workspace={workspace} repo={lane.repo} entry={e} onAnswer={onReview} />}
    </div>
  );
}

/** What the queue did with a queued PR: its runs, their cost, and its events. */
export function QueueActivity({
  workspace,
  lane,
  entry: e,
  onSelect,
}: {
  workspace: number;
  lane: LaneView;
  entry: EntryView;
  /** Selects a PR the events name, when it is queued. */
  onSelect: (pr: number) => void;
}) {
  const events = useQuery({
    queryKey: ["events", workspace, lane.repo, prOf(e)],
    queryFn: () => fetchEvents(workspace, lane.repo, prOf(e)),
    refetchInterval: 3000,
  });
  if (events.isPending) return <p className="text-faint">Loading…</p>;
  if (events.isError) return <p className="text-bad">{events.error.message}</p>;
  if (events.data.length === 0) return <p className="text-faint">Nothing yet.</p>;
  return (
    <Activity
      workspace={workspace}
      events={events.data}
      repo={lane.repo}
      renderMessage={(message) =>
        message.split(/(#\d+)/).map((part, i) =>
          i % 2 ? (
            // biome-ignore lint/suspicious/noArrayIndexKey: the parts of a message never move.
            <PrLink key={i} lane={lane} number={Number(part.slice(1))} onSelect={onSelect} />
          ) : (
            part
          ),
        )
      }
    />
  );
}

/** The queued PR as `PullDetail` reads it from GitHub: a stack's top layer, as its events. */
const queuedPull = (repo: string, e: EntryView): OpenPullView => ({
  ...e,
  number: prOf(e),
  url: e.url || `https://github.com/${repo}/pull/${prOf(e)}`,
  draft: false,
});

/** The lane's entry for a PR, or for the stack it is a layer of. */
export const entryOf = (lane: LaneView, n: number) =>
  lane.entries.find((e) => e.number === n || e.layers.includes(n));

/** A `#n` in an event: selects a queued PR, with its details on hover. Any other PR, such as a
 * batch's draft or one already merged, opens on GitHub. */
function PrLink({
  lane,
  number,
  onSelect,
}: {
  lane: LaneView;
  number: number;
  onSelect: (pr: number) => void;
}) {
  const [at, setAt] = useState<{ top: number; left: number } | null>(null);
  const entry = lane.entries.find((e) => e.number === number);
  const link =
    "text-fg-2 underline decoration-line-strong underline-offset-2 hover:text-fg hover:decoration-fg";
  if (!entry)
    return (
      <a
        href={`https://github.com/${lane.repo}/pull/${number}`}
        target="_blank"
        rel="noreferrer"
        className={link}
      >
        #{number}
      </a>
    );
  const show = (el: HTMLElement) => {
    const r = el.getBoundingClientRect();
    setAt({ top: r.bottom + 6, left: Math.min(r.left, window.innerWidth - 328) });
  };
  return (
    <>
      <button
        type="button"
        className={`${link} cursor-pointer`}
        onClick={() => onSelect(number)}
        onMouseEnter={(ev) => show(ev.currentTarget)}
        onFocus={(ev) => show(ev.currentTarget)}
        onMouseLeave={() => setAt(null)}
        onBlur={() => setAt(null)}
      >
        #{number}
      </button>
      {/* Fixed, so the scrolling pane does not clip it. */}
      {at && (
        <span
          role="tooltip"
          style={at}
          className="pointer-events-none fixed z-50 block w-80 rounded-lg border border-line bg-surface p-3 font-sans text-sm whitespace-normal text-fg shadow-lg"
        >
          <span className="flex items-start justify-between gap-3">
            <span className="font-medium">{titleOf(entry)}</span>
            <State entry={entry} />
          </span>
          <span className="mt-1.5 flex items-center gap-2 text-xs text-muted">
            <span className="tabular-nums">#{entry.number}</span>
            {entry.author && (
              <span className="flex items-center gap-1">
                <Avatar src={entry.author.avatar_url} alt={entry.author.login} size="size-4" />
                {entry.author.login}
              </span>
            )}
            <span className="ml-auto flex items-center gap-2">
              <Checks checks={entry.checks} />
              <Diff entry={entry} />
            </span>
          </span>
        </span>
      )}
    </>
  );
}
