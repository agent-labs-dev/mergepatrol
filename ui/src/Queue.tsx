import { useQuery } from "@tanstack/react-query";
import { useNavigate, useSearch } from "@tanstack/react-router";
import { type ReactNode, useState } from "react";
import { fetchEvents, fetchLanes } from "./api";
import type { ChecksView } from "./api/ChecksView";
import type { EntryView } from "./api/EntryView";
import type { LaneView } from "./api/LaneView";
import { Avatar, secondary, timeAgo } from "./ui";

const laneKey = (l: LaneView) => `${l.repo}:${l.trunk}`;

/** Three panes: the lanes, the selected lane's PRs in merge order, the selected PR. */
export function Queue({ workspace }: { workspace: number }) {
  const { lane: laneId, pr } = useSearch({ from: "/$account/queue" });
  const navigate = useNavigate({ from: "/$account/queue" });
  const setPr = (pr: number) => navigate({ search: (s) => ({ ...s, pr }) });
  const lanes = useQuery({
    queryKey: ["lanes", workspace],
    queryFn: () => fetchLanes(workspace),
    refetchInterval: 3000,
  });

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
  const entry = lane.entries.find((e) => e.number === pr) ?? lane.entries[0];
  // The queue lists the active PRs first, so the batch is the head of the lane.
  const batched = lane.batch === null ? [] : lane.entries.filter((e) => e.state === "active");
  const rest = lane.entries.slice(batched.length);
  const row = (e: EntryView) => (
    <Row
      entry={e}
      place={e.state === "held" ? null : lane.entries.indexOf(e) + 1}
      selected={e === entry}
      onClick={() => setPr(e.number)}
    />
  );
  return (
    <div className="grid h-full grid-cols-[220px_minmax(360px,1fr)_minmax(400px,1.1fr)]">
      <div className="overflow-y-auto border-r border-line p-2">
        <p className="px-2 py-2 text-xs font-medium text-muted">Queues</p>
        {lanes.data.map((l) => (
          <button
            key={laneKey(l)}
            type="button"
            onClick={() => navigate({ search: { lane: laneKey(l) } })}
            className={`flex w-full items-center justify-between rounded-md px-2 py-1.5 text-left ${
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
      <div className="overflow-y-auto border-r border-line bg-surface">
        <div className="sticky top-0 border-b border-line bg-surface px-4 py-3">
          <p className="font-medium">{lane.repo}</p>
          <p className="text-xs text-muted">Merge order into {lane.trunk}</p>
        </div>
        {lane.entries.length === 0 && <p className="p-4 text-faint">Empty</p>}
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
      </div>
      <div className="overflow-y-auto">
        {entry && <Detail workspace={workspace} lane={lane} entry={entry} onSelect={setPr} />}
      </div>
    </div>
  );
}

const stateBadge: Record<EntryView["state"], [string, string]> = {
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

function State({ entry }: { entry: EntryView }) {
  const [label, style] = stateBadge[entry.state];
  return (
    <span className={`rounded-md px-1.5 py-0.5 text-xs font-medium ring-1 ring-inset ${style}`}>
      {label}
    </span>
  );
}

/** A PR saved before the queue kept its details has no title until the next tick. */
const titleOf = (e: EntryView) => e.title || `Pull request #${e.number}`;

function Row({
  entry: e,
  place,
  selected,
  onClick,
}: {
  entry: EntryView;
  /** `null` for a held PR: it has no place until it is released. */
  place: number | null;
  selected: boolean;
  onClick: () => void;
}) {
  return (
    <button
      type="button"
      onClick={onClick}
      className={`flex w-full gap-3 border-b border-line-soft px-4 py-3 text-left ${
        selected ? "bg-page shadow-[inset_2px_0_0_0_var(--color-fg)]" : "hover:bg-page"
      }`}
    >
      <span className="w-4 pt-px text-right text-xs text-faint tabular-nums">{place}</span>
      <span className="min-w-0 flex-1">
        <span className="flex items-start justify-between gap-3">
          <span className="truncate font-medium">{titleOf(e)}</span>
          <State entry={e} />
        </span>
        <span className="mt-1 flex items-center gap-2 text-xs text-muted">
          <span className="tabular-nums">#{e.number}</span>
          {e.author && (
            <span className="flex items-center gap-1">
              <Avatar src={e.author.avatar_url} alt={e.author.login} size="size-4" />
              {e.author.login}
            </span>
          )}
          {e.priority && <span className="font-medium text-hot">↑ Priority</span>}
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

function Diff({ entry: e }: { entry: EntryView }) {
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
}: {
  workspace: number;
  lane: LaneView;
  entry: EntryView;
  onSelect: (pr: number) => void;
}) {
  const events = useQuery({
    queryKey: ["events", workspace, lane.repo, e.number],
    queryFn: () => fetchEvents(workspace, lane.repo, e.number),
    refetchInterval: 3000,
  });
  const url = e.url || `https://github.com/${lane.repo}/pull/${e.number}`;
  return (
    <div className="p-6">
      <div className="flex items-start justify-between gap-4">
        <div className="min-w-0">
          <h2 className="text-lg font-semibold tracking-tight">{titleOf(e)}</h2>
          <p className="mt-0.5 text-muted">
            {lane.repo} #{e.number}
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
      <ol className="overflow-hidden rounded-lg border border-line bg-surface empty:hidden">
        {events.data?.map((ev) => (
          // Events have no ID in the API yet.
          <li
            key={`${ev.at} ${ev.kind}`}
            className="flex items-baseline gap-3 border-line-soft px-4 py-2.5 not-first:border-t"
          >
            <span className="w-20 shrink-0 font-medium capitalize">{ev.kind}</span>
            <span className="min-w-0 flex-1 font-mono text-xs break-words whitespace-pre-wrap text-muted">
              {ev.message.split(/(#\d+)/).map((part, i) =>
                i % 2 ? (
                  // biome-ignore lint/suspicious/noArrayIndexKey: the parts of a message never move.
                  <PrLink key={i} lane={lane} number={Number(part.slice(1))} onSelect={onSelect} />
                ) : (
                  part
                ),
              )}
            </span>
            <time className="shrink-0 text-xs text-faint" title={ev.at}>
              {timeAgo(ev.at)}
            </time>
          </li>
        ))}
      </ol>
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

function Reviews({ entry: e }: { entry: EntryView }) {
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
