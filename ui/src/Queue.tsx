import { useQuery } from "@tanstack/react-query";
import { useNavigate, useSearch } from "@tanstack/react-router";
import { type ReactNode, useState } from "react";
import { Activity } from "./Activity";
import { fetchEvents, fetchLanes, fetchOpenPulls } from "./api";
import type { ChecksView } from "./api/ChecksView";
import type { EntryView } from "./api/EntryView";
import type { LaneView } from "./api/LaneView";
import {
  type Actions,
  asItem,
  BulkBar,
  DetailActions,
  type Item,
  PrototypeSwitcher,
  RowActions,
  useActions,
} from "./QueuePrototype";
import { Avatar, secondary, timeAgo } from "./ui";

const laneKey = (l: LaneView) => `${l.repo}:${l.trunk}`;

/** Three panes: the lanes, the selected lane's PRs in merge order, the selected PR. A narrow
 * screen shows the lanes and the PRs until a PR is picked, then the PR alone. */
export function Queue({ workspace }: { workspace: number }) {
  const { lane: laneId, pr, variant = "A" } = useSearch({ from: "/$account/queue" });
  const navigate = useNavigate({ from: "/$account/queue" });
  const setPr = (pr?: number) => navigate({ search: (s) => ({ ...s, pr }) });
  const lanes = useQuery({
    queryKey: ["lanes", workspace],
    queryFn: () => fetchLanes(workspace),
    refetchInterval: 3000,
  });
  // PROTOTYPE: the lane's open PRs, the actions on them, and variant C's checked rows.
  const current = lanes.data?.find((l) => laneKey(l) === laneId) ?? lanes.data?.[0];
  const openPulls = useQuery({
    queryKey: ["open", workspace, current?.repo],
    queryFn: () => fetchOpenPulls(workspace, current?.repo ?? ""),
    enabled: current !== undefined,
    refetchInterval: 3000,
  });
  const actions = useActions(workspace, current?.repo ?? "");
  const [checked, setChecked] = useState(new Set<number>());

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

  const lane = lanes.data.find((l) => laneKey(l) === laneId) ?? lanes.data[0];
  const open = (openPulls.data ?? []).map(asItem);
  const items: Item[] = [...lane.entries, ...open];
  const entry = items.find((e) => e.number === pr) ?? items[0];
  // The queue lists the active PRs first, so the batch is the head of the lane.
  const batched = lane.batch === null ? [] : lane.entries.filter((e) => e.state === "active");
  const rest = lane.entries.slice(batched.length);
  const picked = entry !== undefined && entry.number === pr;
  const toggle = (n: number) => {
    const next = new Set(checked);
    if (!next.delete(n)) next.add(n);
    setChecked(next);
  };
  const row = (e: Item) => (
    <div
      className={`group flex items-stretch border-b border-line-soft hover:bg-page ${e === entry ? "lg:bg-page" : ""}`}
    >
      {variant === "C" && (
        <label className="flex shrink-0 cursor-pointer items-start py-3.5 pl-4">
          <input
            type="checkbox"
            checked={checked.has(e.number)}
            onChange={() => toggle(e.number)}
            className="size-3.5 accent-fg"
          />
        </label>
      )}
      <Row
        entry={e}
        place={
          e.state === "held" || e.state === "open" ? null : lane.entries.indexOf(e as EntryView) + 1
        }
        selected={e === entry}
        onClick={() => setPr(e.number)}
      />
      {variant === "A" && <RowActions item={e} actions={actions} />}
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
            onClick={() => navigate({ search: (s) => ({ variant: s.variant, lane: laneKey(l) }) })}
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
        {open.length > 0 && (
          <>
            <p className="mt-6 flex items-center justify-between border-y border-line bg-sunken px-4 py-2 text-xs text-muted">
              <span>
                <span className="font-medium text-fg-2">Open</span> · not queued
              </span>
              <span className="tabular-nums">{open.length}</span>
            </p>
            <ol className={variant === "B" ? "opacity-75 hover:opacity-100" : ""}>
              {open.map((e) => (
                <li key={e.number}>{row(e)}</li>
              ))}
            </ol>
          </>
        )}
        {variant === "C" && (
          <BulkBar items={items} checked={checked} setChecked={setChecked} actions={actions} />
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
            actions={variant === "B" ? actions : undefined}
          />
        )}
      </div>
      <PrototypeSwitcher current={variant} />
    </div>
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
  onClick,
}: {
  entry: Item;
  /** `null` for a held PR: it has no place until it is released. */
  place: number | null;
  selected: boolean;
  onClick: () => void;
}) {
  return (
    <button
      type="button"
      onClick={onClick}
      // A narrow screen lists the PRs only while none is open, so none shows as selected there.
      className={`flex min-w-0 flex-1 gap-3 px-4 py-3 text-left hover:bg-page ${
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
        {e.detail && <span className="mt-1 block text-warn">{e.detail}</span>}
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
  actions,
}: {
  workspace: number;
  lane: LaneView;
  entry: Item;
  onSelect: (pr: number) => void;
  /** Back to the PRs, on a screen too narrow to show them beside it. */
  onBack: () => void;
  actions?: Actions;
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
      {actions && (
        <div className="mt-4 flex flex-wrap gap-2 rounded-lg border border-line bg-surface p-2">
          <DetailActions item={e} actions={actions} />
        </div>
      )}
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
