/*
 * PROTOTYPE, throw away. Three variants of the Queue page's PR list, switchable via `?variant=`
 * on the existing `/$account/queue` route. Run with `pnpm prototype` (fake API, no server).
 *
 * Question: how should the page show a repo's open PRs that are not queued, below the queue,
 * each with Queue and Priority queue, and Remove from queue on every queued PR?
 *
 * - A (Inline buttons): actions sit on every row. Open PRs get Queue / ↑ Priority, queued PRs
 *   reveal Remove on hover.
 * - B (Detail actions): rows stay clean. Actions live in the selected PR's header.
 * - C (Bulk select): checkboxes on every row, a sticky bar acts on the selection.
 */
import { useMutation, useQueryClient } from "@tanstack/react-query";
import { useNavigate } from "@tanstack/react-router";
import { useEffect } from "react";
import { dequeuePull, type OpenPull, queuePull } from "./api";
import type { EntryView } from "./api/EntryView";
import { danger, primary, secondary } from "./ui";

export const VARIANTS = {
  A: "Inline buttons",
  B: "Detail actions",
  C: "Bulk select",
} as const;
export type Variant = keyof typeof VARIANTS;
export const variantOf = (v: unknown): Variant | undefined =>
  typeof v === "string" && v in VARIANTS ? (v as Variant) : undefined;

/** An open PR, shaped as an entry so the queue's row and detail render it. */
export type Item = Omit<EntryView, "state"> & { state: EntryView["state"] | "open" };

export const asItem = (p: OpenPull): Item => ({
  ...p,
  priority: false,
  state: "open",
  // What the queue would do with it, so nobody queues a draft expecting it to run.
  detail: p.draft
    ? "Draft: would be held until marked ready."
    : p.review_decision === "changes_requested"
      ? "Changes requested: would be held."
      : p.checks === "failed"
        ? "Checks failing: the queue would try a fix."
        : null,
});

export function useActions(workspace: number, repo: string) {
  const client = useQueryClient();
  const done = () => client.invalidateQueries({ predicate: (q) => q.queryKey[0] !== "session" });
  const queue = useMutation({
    mutationFn: ({ pr, priority }: { pr: number; priority: boolean }) =>
      queuePull(workspace, repo, pr, priority),
    onSuccess: done,
  });
  const remove = useMutation({
    mutationFn: (pr: number) => dequeuePull(workspace, repo, pr),
    onSuccess: done,
  });
  return {
    busy: queue.isPending || remove.isPending,
    queue: (pr: number, priority = false) => queue.mutate({ pr, priority }),
    remove: (pr: number) => remove.mutate(pr),
    queueAll: (prs: number[], priority: boolean) =>
      Promise.all(prs.map((pr) => queuePull(workspace, repo, pr, priority))).then(done),
    removeAll: (prs: number[]) =>
      Promise.all(prs.map((pr) => dequeuePull(workspace, repo, pr))).then(done),
  };
}
export type Actions = ReturnType<typeof useActions>;

const small =
  "inline-flex h-7 items-center rounded-md border border-line-strong bg-surface px-2.5 text-xs font-medium shadow-xs hover:bg-page disabled:opacity-40";

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

/** Variant A: two icon slots at the end of every row, the same width on every row so nothing
 * shifts: priority, then queue or remove, on the title's line. A queued row's icons are faint
 * until hover, except an active priority. */
export function RowActions({ item, actions }: { item: Item; actions: Actions }) {
  const { busy } = actions;
  const quiet = "text-faint/60 group-hover:text-muted group-focus-within:text-muted";
  if (item.state === "open")
    return (
      <span className="flex shrink-0 items-start gap-0.5 pt-2.5 pr-3">
        <IconButton
          icon="priority"
          label="Priority queue"
          disabled={busy}
          onClick={() => actions.queue(item.number, true)}
          className="text-muted hover:text-hot"
        />
        <IconButton
          icon="queue"
          label="Queue"
          disabled={busy}
          onClick={() => actions.queue(item.number)}
          className="text-muted hover:text-fg"
        />
      </span>
    );
  return (
    <span className="flex shrink-0 items-start gap-0.5 pt-2.5 pr-3">
      <IconButton
        icon="priority"
        label={item.priority ? "Drop priority" : "Make priority"}
        disabled={busy}
        onClick={() => actions.queue(item.number, !item.priority)}
        className={item.priority ? "text-hot" : `${quiet} hover:text-hot!`}
      />
      <IconButton
        icon="remove"
        label="Remove from queue"
        disabled={busy}
        onClick={() => actions.remove(item.number)}
        className={`${quiet} hover:text-bad!`}
      />
    </span>
  );
}

/** Variant B: the buttons in the selected PR's header. */
export function DetailActions({ item, actions }: { item: Item; actions: Actions }) {
  if (item.state === "open")
    return (
      <>
        <button
          type="button"
          disabled={actions.busy}
          onClick={() => actions.queue(item.number, true)}
          className={`${secondary} text-hot`}
        >
          ↑ Priority queue
        </button>
        <button
          type="button"
          disabled={actions.busy}
          onClick={() => actions.queue(item.number)}
          className={primary}
        >
          Queue
        </button>
      </>
    );
  return (
    <>
      <button
        type="button"
        disabled={actions.busy}
        onClick={() => actions.queue(item.number, !item.priority)}
        className={secondary}
      >
        {item.priority ? "Drop priority" : "↑ Make priority"}
      </button>
      <button
        type="button"
        disabled={actions.busy}
        onClick={() => actions.remove(item.number)}
        className={danger}
      >
        Remove from queue
      </button>
    </>
  );
}

/** Variant C: the bar under the list while PRs are checked. */
export function BulkBar({
  items,
  checked,
  setChecked,
  actions,
}: {
  items: Item[];
  checked: Set<number>;
  setChecked: (s: Set<number>) => void;
  actions: Actions;
}) {
  const picked = items.filter((i) => checked.has(i.number));
  const open = picked.filter((i) => i.state === "open").map((i) => i.number);
  const queued = picked.filter((i) => i.state !== "open").map((i) => i.number);
  if (picked.length === 0) return null;
  const run = (p: Promise<unknown>) => p.then(() => setChecked(new Set()));
  return (
    <div className="sticky bottom-0 flex flex-wrap items-center gap-2 border-t border-line bg-surface px-4 py-2.5 shadow-[0_-4px_12px_-6px_rgb(0_0_0/0.15)]">
      <span className="mr-auto text-xs text-muted">
        {picked.length} selected
        <button
          type="button"
          onClick={() => setChecked(new Set())}
          className="ml-2 underline underline-offset-2 hover:text-fg"
        >
          Clear
        </button>
      </span>
      {open.length > 0 && (
        <>
          <button
            type="button"
            onClick={() => run(actions.queueAll(open, false))}
            className={`${small} text-fg`}
          >
            Queue {open.length}
          </button>
          <button
            type="button"
            onClick={() => run(actions.queueAll(open, true))}
            className={`${small} text-hot`}
          >
            ↑ Priority queue {open.length}
          </button>
        </>
      )}
      {queued.length > 0 && (
        <button
          type="button"
          onClick={() => run(actions.removeAll(queued))}
          className={`${small} text-bad`}
        >
          Remove {queued.length} from queue
        </button>
      )}
    </div>
  );
}

/** Not part of any design: flips between the variants. Dev builds only. */
export function PrototypeSwitcher({ current }: { current: Variant }) {
  const navigate = useNavigate({ from: "/$account/queue" });
  const keys = Object.keys(VARIANTS) as Variant[];
  const go = (step: number) => {
    const next = keys[(keys.indexOf(current) + step + keys.length) % keys.length];
    navigate({ search: (s) => ({ ...s, variant: next }), replace: true });
  };
  useEffect(() => {
    const onKey = (e: KeyboardEvent) => {
      const t = e.target as HTMLElement;
      if (t.closest("input, textarea, select, [contenteditable]")) return;
      if (e.key === "ArrowLeft") go(-1);
      if (e.key === "ArrowRight") go(1);
    };
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
  });
  if (!import.meta.env.DEV) return null;
  return (
    <div className="fixed right-5 bottom-5 z-50 flex items-center gap-1 rounded-full bg-fuchsia-600 p-1 text-sm font-medium text-white shadow-xl ring-4 ring-fuchsia-600/25">
      <button
        type="button"
        onClick={() => go(-1)}
        className="rounded-full px-3 py-1 hover:bg-white/20"
      >
        ←
      </button>
      <span className="px-2 tabular-nums">
        {current} <span className="opacity-80">({VARIANTS[current]})</span>
      </span>
      <button
        type="button"
        onClick={() => go(1)}
        className="rounded-full px-3 py-1 hover:bg-white/20"
      >
        →
      </button>
    </div>
  );
}
