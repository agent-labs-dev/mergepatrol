import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import { useNavigate, useSearch } from "@tanstack/react-router";
import { type ReactNode, useEffect, useState } from "react";
import { Activity } from "./Activity";
import { dequeuePull, fetchEvents, fetchLanes, fetchOpenPulls, queuePull } from "./api";
import type { ChecksView } from "./api/ChecksView";
import type { EntryView } from "./api/EntryView";
import type { LaneView } from "./api/LaneView";
import type { OpenPullView } from "./api/OpenPullView";
import { Avatar, secondary, timeAgo } from "./ui";

const laneKey = (l: LaneView) => `${l.repo}:${l.trunk}`;

/** Three panes: the lanes, the selected lane's PRs in merge order, the selected PR. A narrow
 * screen shows the lanes and the PRs until a PR is picked, then the PR alone. */
export function Queue({ workspace }: { workspace: number }) {
  const { lane: laneId, pr } = useSearch({ from: "/$account/queue" });
  const navigate = useNavigate({ from: "/$account/queue" });
  const setPr = (pr?: number) => navigate({ search: (s) => ({ ...s, pr }) });
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
    refetchInterval: 10_000,
  });
  const actions = useActions(workspace, current);

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
  const unqueued = [
    ...(open.data ?? []).map(asItem),
    // Queued, gone from the open PRs, and not in the lane yet: a poll of each came between.
    ...[...pending.values()].flatMap((p) =>
      p.kind === "queueing" && !open.data?.some((o) => o.number === p.item.number) ? [p.item] : [],
    ),
  ].filter((i) => !entryOf(lane, i.number));
  const items: Item[] = [...lane.entries, ...unqueued];
  const entry = items.find((e) => e.number === pr) ?? items[0];
  // The queue lists the active PRs first, so the batch is the head of the lane.
  const batched = lane.batch === null ? [] : lane.entries.filter((e) => e.state === "active");
  const rest = lane.entries.slice(batched.length);
  const picked = entry !== undefined && entry.number === pr;
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
            : lane.entries.findIndex((q) => q.number === e.number) + 1
        }
        selected={e === entry}
        pending={pending.get(e.number)?.kind}
        onClick={() => setPr(e.number)}
      />
      <RowActions item={e} actions={actions} busy={pending.has(e.number)} />
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
            onClick={() => navigate({ search: { lane: laneKey(l) } })}
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
        {lane.entries.length === 0 && <p className="p-4 text-faint">Nothing queued</p>}
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
        {unqueued.length > 0 && (
          <>
            <p className="mt-6 flex items-center justify-between border-y border-line bg-sunken px-4 py-2 text-xs text-muted">
              <span>
                <span className="font-medium text-fg-2">Open</span> · not queued
              </span>
              <span className="tabular-nums">{unqueued.length}</span>
            </p>
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
          <Detail
            workspace={workspace}
            lane={lane}
            entry={entry}
            onSelect={setPr}
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
  detail: p.draft
    ? "A draft: the queue holds it until it is ready."
    : p.review_decision === "changes_requested"
      ? "Changes requested: the queue holds it until they are approved."
      : null,
});

/** A click the queue has not caught up with: GitHub has the labels, the lane not yet. */
type Pending = { kind: "queueing" | "removing"; item: Item };

/** The queue looks at a repo within seconds of a label change. Past this, stop waiting. */
const PENDING_MS = 60_000;

/** The lane's entry for `n`: a PR's, or its stack's. */
const entryOf = (lane: LaneView, n: number) =>
  lane.entries.find((e) => e.number === n || e.layers.includes(n));

function useActions(workspace: number, lane: LaneView | undefined) {
  const client = useQueryClient();
  const [pending, setPending] = useState(new Map<number, Pending & { at: number }>());
  const [error, setError] = useState<string | null>(null);
  const repo = lane?.repo ?? "";
  const drop = (n: number) =>
    setPending((p) => {
      const next = new Map(p);
      next.delete(n);
      return next;
    });
  // Each poll of the lane may show what a click did. Then the open PRs change too.
  useEffect(() => {
    if (!lane) return;
    setPending((p) => {
      const next = new Map(
        [...p].filter(([n, { kind, item, at }]) => {
          const e = entryOf(lane, n);
          const done = kind === "removing" ? !e : e?.priority === item.priority;
          return !done && Date.now() - at < PENDING_MS;
        }),
      );
      if (next.size === p.size) return p;
      client.invalidateQueries({ queryKey: ["open"] });
      return next;
    });
  }, [lane, client]);
  const mutation = useMutation({
    mutationFn: ({ kind, item }: Pending) =>
      kind === "removing"
        ? dequeuePull(workspace, repo, prOf(item))
        : queuePull(workspace, repo, prOf(item), item.priority),
    onMutate: ({ kind, item }) => {
      setError(null);
      setPending((p) => new Map(p).set(item.number, { kind, item, at: Date.now() }));
    },
    onError: (e, { item }) => {
      setError(e.message);
      drop(item.number);
    },
    // The open PRs stay as they are, the clicked one in its place, until the lane has it.
    onSuccess: () => client.invalidateQueries({ queryKey: ["lanes"] }),
  });
  return {
    pending: pending as ReadonlyMap<number, Pending>,
    error,
    dismiss: () => setError(null),
    queue: (item: Item, priority: boolean) =>
      mutation.mutate({ kind: "queueing", item: { ...item, priority } }),
    remove: (item: Item) => mutation.mutate({ kind: "removing", item }),
  };
}
type Actions = ReturnType<typeof useActions>;

const icons = {
  // Two chevrons up: to the front of the queue.
  priority: "M4 8.5 8 4.5l4 4M4 12.5l4-4 4 4",
  queue: "M8 3.5v9M3.5 8h9",
  remove: "M4.5 4.5l7 7M11.5 4.5l-7 7",
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

/** Two slots at the end of every row, so every row lines up: priority, then queue or remove.
 * A queued PR's are faint until hovered, but for a priority it has. */
function RowActions({ item, actions, busy }: { item: Item; actions: Actions; busy: boolean }) {
  const quiet = "text-faint/60 group-hover:text-muted group-focus-within:text-muted";
  const open = item.state === "open";
  return (
    <span className="flex shrink-0 items-start gap-0.5 pt-2.5 pr-3">
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
  const [label, style] = stateBadge[entry.state];
  return (
    <span className={`rounded-md px-1.5 py-0.5 text-xs font-medium ring-1 ring-inset ${style}`}>
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
  pending?: Pending["kind"];
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
            {pending === "removing"
              ? "Leaving the queue…"
              : e.state === "open"
                ? "Queueing…"
                : "Changing its priority…"}
          </span>
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

function Detail({
  workspace,
  lane,
  entry: e,
  onSelect,
  onBack,
}: {
  workspace: number;
  lane: LaneView;
  entry: Item;
  onSelect: (pr: number) => void;
  /** Back to the PRs, on a screen too narrow to show them beside it. */
  onBack: () => void;
}) {
  const events = useQuery({
    queryKey: ["events", workspace, lane.repo, prOf(e)],
    queryFn: () => fetchEvents(workspace, lane.repo, prOf(e)),
    refetchInterval: 3000,
  });
  const url = e.url || `https://github.com/${lane.repo}/pull/${prOf(e)}`;
  return (
    <div className="p-4 sm:p-6">
      <button type="button" onClick={onBack} className="mb-3 text-muted hover:text-fg lg:hidden">
        ← {lane.repo.split("/")[1]}
      </button>
      <div className="flex flex-col gap-3 sm:flex-row sm:items-start sm:justify-between sm:gap-4">
        <div className="min-w-0">
          <h2 className="text-lg font-semibold tracking-tight">{titleOf(e)}</h2>
          <p className="mt-0.5 text-muted">
            {lane.repo} {refsOf(e)}
            {e.author && e.created_at && (
              <>
                {" "}
                · opened {timeAgo(e.created_at)} by {e.author.login}
              </>
            )}
          </p>
        </div>
        <a href={url} target="_blank" rel="noreferrer" className={`${secondary} shrink-0`}>
          Open on GitHub
        </a>
      </div>
      {e.state === "held" && (
        <div className="mt-4 rounded-lg border border-amber-200 bg-amber-50 px-4 py-3 text-amber-900 dark:border-amber-400/30 dark:bg-amber-400/10 dark:text-amber-200">
          <p className="font-medium">Held</p>
          <p className="mt-0.5">{e.detail}</p>
        </div>
      )}
      {e.url && (
        <dl className="mt-6 grid grid-cols-[110px_1fr] gap-x-4 gap-y-3">
          <Field label="Status">
            <span className="flex items-center gap-2">
              <State entry={e} />
              {e.priority && <span className="text-xs font-medium text-hot">↑ Priority</span>}
              {e.fork && <span className="text-xs text-muted">From a fork</span>}
            </span>
          </Field>
          <Field label="Branch">
            <span className="font-mono text-xs">
              {e.head_ref} <span className="text-faint">→ {lane.trunk}</span>
            </span>
          </Field>
          <Field label="Checks">
            <Checks checks={e.checks} label />
          </Field>
          <Field label="Reviews">
            <Reviews entry={e} />
          </Field>
          <Field label="Changes">
            <span>
              <Diff entry={e} />
              <span className="ml-2 text-muted">
                {e.changed_files} {e.changed_files === 1 ? "file" : "files"}
              </span>
            </span>
          </Field>
        </dl>
      )}
      <h3 className="mt-8 mb-3 text-xs font-medium text-muted">Activity</h3>
      {events.data?.length === 0 && <p className="text-faint">Nothing yet.</p>}
      {!!events.data?.length && (
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
      )}
    </div>
  );
}

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

function Field({ label, children }: { label: string; children: ReactNode }) {
  return (
    <>
      <dt className="text-muted">{label}</dt>
      <dd className="min-w-0">{children}</dd>
    </>
  );
}

const decision: Record<NonNullable<EntryView["review_decision"]>, [string, string]> = {
  approved: ["Approved", "text-ok"],
  review_required: ["Review required", "text-warn"],
  changes_requested: ["Changes requested", "text-bad"],
};

function Reviews({ entry: e }: { entry: Item }) {
  const [text, color] = e.review_decision
    ? decision[e.review_decision]
    : ["Not required", "text-muted"];
  return (
    <div className="space-y-2">
      <p className={color}>{text}</p>
      {(e.reviews.length > 0 || e.requested.length > 0) && (
        <ul className="flex flex-wrap gap-x-4 gap-y-1.5">
          {e.reviews.map((r) => (
            <li key={r.reviewer.login} className="flex items-center gap-1.5">
              <Avatar src={r.reviewer.avatar_url} alt={r.reviewer.login} size="size-4" />
              {r.reviewer.login}
              {r.approved ? (
                <span className="text-ok" title="Approved">
                  ✓
                </span>
              ) : (
                <span className="text-bad" title="Requested changes">
                  ✕
                </span>
              )}
            </li>
          ))}
          {e.requested.map((p) => (
            <li key={p.login} className="flex items-center gap-1.5 text-muted">
              <Avatar src={p.avatar_url} alt={p.login} size="size-4" />
              {p.login}
              <span className="text-xs" title="Review requested">
                pending
              </span>
            </li>
          ))}
        </ul>
      )}
    </div>
  );
}
