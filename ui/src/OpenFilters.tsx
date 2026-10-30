import { type ReactNode, useEffect, useRef, useState } from "react";
import type { OpenPullView } from "./api/OpenPullView";
import { Avatar } from "./ui";

/*
 * Filters on the open PRs, never the queue: the queue's order is the point of the page. They live
 * in the Queue page's search params, so a reload or a link keeps them. Unset is the default, and
 * hides the drafts, which the queue can't take.
 */

export const STATUSES = ["ready", "failing", "running", "changes"] as const;
export type Status = (typeof STATUSES)[number];
export const REVIEWS = ["mine", "needed", "approved"] as const;
export type Review = (typeof REVIEWS)[number];

export type OpenFilter = {
  /** A GitHub login. */
  author?: string;
  status?: Status;
  review?: Review;
  drafts?: boolean;
  /** Matches the title, the `#number` and the branch. */
  q?: string;
};

const statusLabels: Record<Status, string> = {
  ready: "Ready to merge",
  failing: "Checks failing",
  running: "Checks running",
  changes: "Changes requested",
};

const reviewLabels: Record<Review, string> = {
  mine: "Requested from you",
  needed: "Needs review",
  approved: "Approved",
};

/** A deleted account's PRs: GitHub shows them as `ghost`. */
const GHOST = "ghost";
const loginOf = (p: OpenPullView) => p.author?.login ?? GHOST;
/** Listed apart, after the people. */
const isBot = (login: string) => login === GHOST || login.endsWith("[bot]");

const byStatus: Record<Status, (p: OpenPullView) => boolean> = {
  ready: (p) =>
    !p.draft &&
    (p.checks === "passed" || p.checks === null) &&
    (p.review_decision === "approved" || p.review_decision === null),
  failing: (p) => p.checks === "failed",
  running: (p) => p.checks === "pending",
  changes: (p) => p.review_decision === "changes_requested",
};

const byReview = (me: string | undefined): Record<Review, (p: OpenPullView) => boolean> => ({
  mine: (p) => p.requested.some((r) => r.login === me),
  needed: (p) => p.review_decision === "review_required",
  approved: (p) => p.review_decision === "approved",
});

/** Whether the filter narrows the PRs at all, beyond hiding the drafts. */
export const narrows = (f: OpenFilter) => !!(f.author || f.status || f.review || f.q);

export function matches(f: OpenFilter, me: string | undefined) {
  const q = f.q?.trim().toLowerCase();
  return (p: OpenPullView) =>
    (f.drafts || !p.draft) &&
    (!f.author || loginOf(p) === f.author) &&
    (!f.status || byStatus[f.status](p)) &&
    (!f.review || byReview(me)[f.review](p)) &&
    (!q ||
      p.title.toLowerCase().includes(q) ||
      `#${p.number}`.includes(q) ||
      p.head_ref.toLowerCase().includes(q));
}

type Author = { login: string; avatar_url: string | null; count: number };

/** The open PRs' authors, by how many each has open: you first, even with none, and bots last.
 * Not the repo's contributors, since one without an open PR can only show an empty list. */
function authorsOf(pulls: OpenPullView[], me: string | undefined): Author[] {
  const by = new Map<string, Author>();
  if (me) by.set(me, { login: me, avatar_url: null, count: 0 });
  for (const p of pulls) {
    const login = loginOf(p);
    const a = by.get(login) ?? { login, avatar_url: null, count: 0 };
    a.count++;
    a.avatar_url ??= p.author?.avatar_url ?? null;
    by.set(login, a);
  }
  const rank = (a: Author) => (a.login === me ? 0 : isBot(a.login) ? 2 : 1);
  return [...by.values()].sort(
    (a, b) => rank(a) - rank(b) || b.count - a.count || a.login.localeCompare(b.login),
  );
}

export function OpenFilters({
  pulls,
  me,
  filter: f,
  onChange,
}: {
  pulls: OpenPullView[];
  me: string | undefined;
  filter: OpenFilter;
  onChange: (f: OpenFilter) => void;
}) {
  const set = (patch: OpenFilter) => onChange({ ...f, ...patch });
  // The drafts the other filters would show.
  const drafts = pulls.filter(matches({ ...f, drafts: true }, me)).filter((p) => p.draft).length;
  return (
    <div className="flex flex-wrap items-center gap-1.5 border-b border-line-soft px-4 py-2">
      <AuthorMenu
        authors={authorsOf(pulls, me)}
        me={me}
        selected={f.author}
        onPick={(author) => set({ author })}
      />
      <Menu
        label="Status"
        any="Any status"
        labels={statusLabels}
        value={f.status}
        onPick={(status) => set({ status })}
        footer={
          <label className="flex items-center gap-2 border-t border-line px-3 py-2 text-xs text-muted">
            <input
              type="checkbox"
              checked={!!f.drafts}
              onChange={(e) => set({ drafts: e.target.checked || undefined })}
            />
            Show drafts
          </label>
        }
      />
      <Menu
        label="Reviews"
        any="Any review"
        labels={reviewLabels}
        value={f.review}
        onPick={(review) => set({ review })}
      />
      <input
        value={f.q ?? ""}
        onChange={(e) => set({ q: e.target.value || undefined })}
        placeholder="Search title, #, branch"
        aria-label="Search the open PRs"
        className="h-7 min-w-32 flex-1 rounded-md bg-transparent px-2 text-xs ring-1 ring-line-strong outline-none ring-inset placeholder:text-faint focus:ring-fg"
      />
      {!f.drafts && drafts > 0 && (
        <button
          type="button"
          onClick={() => set({ drafts: true })}
          className="text-xs text-faint hover:text-fg"
          title="The queue holds a draft until it is ready, so drafts are hidden"
        >
          {drafts} {drafts === 1 ? "draft" : "drafts"} hidden
        </button>
      )}
    </div>
  );
}

/** A menu that closes on a click anywhere else. */
function Popover({
  button,
  width,
  children,
}: {
  button: (toggle: () => void) => ReactNode;
  width: string;
  children: (close: () => void) => ReactNode;
}) {
  const [open, setOpen] = useState(false);
  const ref = useRef<HTMLDivElement>(null);
  useEffect(() => {
    if (!open) return;
    const close = (e: MouseEvent) => {
      if (!ref.current?.contains(e.target as Node)) setOpen(false);
    };
    const onKey = (e: KeyboardEvent) => e.key === "Escape" && setOpen(false);
    document.addEventListener("mousedown", close);
    document.addEventListener("keydown", onKey);
    return () => {
      document.removeEventListener("mousedown", close);
      document.removeEventListener("keydown", onKey);
    };
  }, [open]);
  return (
    <div ref={ref} className="relative">
      {button(() => setOpen(!open))}
      {open && (
        <div
          className={`absolute top-8 left-0 z-30 ${width} overflow-hidden rounded-lg border border-line bg-surface text-sm shadow-lg`}
        >
          {children(() => setOpen(false))}
        </div>
      )}
    </div>
  );
}

const chip = (on: boolean) =>
  `inline-flex h-7 items-center gap-1.5 rounded-md px-2 text-xs ring-1 ring-inset ${
    on ? "bg-fg text-surface ring-fg" : "text-fg-2 ring-line-strong hover:bg-sunken"
  }`;

function Caret() {
  return (
    <svg
      viewBox="0 0 16 16"
      className="size-3 fill-none stroke-current opacity-60"
      strokeWidth="1.75"
      strokeLinecap="round"
      strokeLinejoin="round"
      aria-hidden
    >
      <path d="m4 6 4 4 4-4" />
    </svg>
  );
}

const option = "flex w-full items-center gap-2 px-3 py-1.5 text-left hover:bg-sunken";
const check = <span className="ml-auto text-xs">✓</span>;

function AuthorMenu({
  authors,
  me,
  selected,
  onPick,
}: {
  authors: Author[];
  me: string | undefined;
  selected: string | undefined;
  onPick: (login: string | undefined) => void;
}) {
  const [q, setQ] = useState("");
  const current = authors.find((a) => a.login === selected);
  const shown = authors.filter((a) => a.login.toLowerCase().includes(q.toLowerCase()));
  const people = shown.filter((a) => !isBot(a.login));
  const bots = shown.filter((a) => isBot(a.login));
  return (
    <Popover
      width="w-64"
      button={(toggle) => (
        <button type="button" onClick={toggle} className={chip(!!selected)}>
          {current?.avatar_url && <Avatar src={current.avatar_url} alt="" size="size-4" />}
          {selected ? `Author: ${selected === me ? "you" : selected}` : "Author"}
          <Caret />
        </button>
      )}
    >
      {(close) => {
        const pick = (login: string | undefined) => {
          onPick(login);
          setQ("");
          close();
        };
        const row = (a: Author) => (
          <button key={a.login} type="button" onClick={() => pick(a.login)} className={option}>
            {a.avatar_url ? (
              <Avatar src={a.avatar_url} alt="" size="size-5" />
            ) : (
              <span className="size-5 shrink-0 rounded-full bg-sunken" />
            )}
            <span className="min-w-0 truncate">
              {a.login}
              {a.login === me && <span className="ml-1.5 text-xs text-muted">you</span>}
            </span>
            <span className="ml-auto text-xs text-faint tabular-nums">{a.count}</span>
            {a.login === selected && <span className="text-xs">✓</span>}
          </button>
        );
        return (
          <>
            <input
              // biome-ignore lint/a11y/noAutofocus: the menu opens to type a login into.
              autoFocus
              value={q}
              onChange={(e) => setQ(e.target.value)}
              placeholder="Filter authors"
              aria-label="Filter authors"
              className="w-full border-b border-line bg-transparent px-3 py-2 text-xs outline-none"
            />
            <div className="max-h-80 overflow-y-auto py-1">
              {!q && (
                <button
                  type="button"
                  onClick={() => pick(undefined)}
                  className={`${option} text-muted`}
                >
                  <span className="size-5 shrink-0" />
                  Anyone
                  {!selected && check}
                </button>
              )}
              {people.map(row)}
              {bots.length > 0 && (
                <p className="mt-1 border-t border-line px-3 pt-2 pb-1 text-xs font-medium text-faint">
                  Bots and deleted accounts
                </p>
              )}
              {bots.map(row)}
              {shown.length === 0 && <p className="px-3 py-1.5 text-faint">No one matches</p>}
            </div>
          </>
        );
      }}
    </Popover>
  );
}

function Menu<K extends string>({
  label,
  any,
  labels,
  value,
  onPick,
  footer,
}: {
  label: string;
  /** The first option, which unsets the filter. */
  any: string;
  labels: Record<K, string>;
  value: K | undefined;
  onPick: (value: K | undefined) => void;
  footer?: ReactNode;
}) {
  const keys = Object.keys(labels) as K[];
  return (
    <Popover
      width="w-56"
      button={(toggle) => (
        <button type="button" onClick={toggle} className={chip(!!value)}>
          {value ? labels[value] : label}
          <Caret />
        </button>
      )}
    >
      {(close) => (
        <>
          <div className="py-1">
            {[undefined, ...keys].map((k) => (
              <button
                key={k ?? ""}
                type="button"
                onClick={() => {
                  onPick(k);
                  close();
                }}
                className={`${option} ${k ? "" : "text-muted"}`}
              >
                {k ? labels[k] : any}
                {k === value && check}
              </button>
            ))}
          </div>
          {footer}
        </>
      )}
    </Popover>
  );
}
