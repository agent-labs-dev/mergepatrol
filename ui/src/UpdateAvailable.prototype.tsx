/*
 * PROTOTYPE, throw away. Three variants of an "update available" notice in the signed-in
 * dashboard, switchable via `?variant=A|B|C` and the bar at the bottom. Click reloads the page.
 * The notice always shows here; the real one would show once a newer build is deployed.
 */
import { useEffect, useState } from "react";

export const VARIANTS = ["A", "B", "C"] as const;
export type Variant = (typeof VARIANTS)[number];
export const NAMES: Record<Variant, string> = {
  A: "Header pill",
  B: "Banner under header",
  C: "Toast with what's new",
};

// Stand-ins from the changelog session's content/changelog/.
const whatsNew = [
  { kind: "new", title: "The You tab: your PRs and reviews, across workspaces" },
  { kind: "improved", title: "Close a PR from the queue page" },
  { kind: "fixed", title: "PRs with an empty commit get through the queue" },
];

const reload = () => location.reload();

export function useVariant() {
  const read = () => {
    const v = new URLSearchParams(location.search).get("variant");
    return (VARIANTS as readonly string[]).includes(v ?? "") ? (v as Variant) : "A";
  };
  const [variant, setVariant] = useState<Variant>(read);
  const set = (v: Variant) => {
    const url = new URL(location.href);
    url.searchParams.set("variant", v);
    history.replaceState(history.state, "", url);
    setVariant(v);
  };
  return [variant, set] as const;
}

/** A: a small pill in the header's right-hand group. Quiet, always in reach. */
export function HeaderPill() {
  return (
    <button
      type="button"
      onClick={reload}
      title="A new version of Mergepatrol is out. Reload to get it."
      className="flex items-center gap-1.5 rounded-full border border-line-strong bg-surface px-2.5 py-0.5 text-xs font-medium text-fg-2 shadow-xs hover:bg-page hover:text-fg"
    >
      <span className="relative flex size-2">
        <span className="absolute inline-flex size-full animate-ping rounded-full bg-sky-500 opacity-60" />
        <span className="relative inline-flex size-2 rounded-full bg-sky-500" />
      </span>
      <span className="max-sm:hidden">Update available</span>
      <span className="sm:hidden">Update</span>
    </button>
  );
}

/** B: a full-width strip under the header, like the no-key banner but calmer. */
export function Banner() {
  const [open, setOpen] = useState(true);
  if (!open) return null;
  return (
    <div className="flex items-center gap-3 border-b border-line bg-sunken px-4 py-1.5 text-sm sm:px-5">
      <svg
        viewBox="0 0 16 16"
        className="size-4 shrink-0 fill-none stroke-sky-500"
        strokeWidth="1.5"
        aria-hidden
      >
        <path
          d="M13.5 8a5.5 5.5 0 1 1-1.6-3.9M13.5 2.5v2.5H11"
          strokeLinecap="round"
          strokeLinejoin="round"
        />
      </svg>
      <span className="min-w-0 flex-1 text-fg-2">
        <span className="font-medium text-fg">A new version of Mergepatrol is out.</span>{" "}
        <a
          href="/changelog"
          className="underline decoration-line-strong underline-offset-2 hover:text-fg"
        >
          See what's new
        </a>
      </span>
      <button
        type="button"
        onClick={reload}
        className="shrink-0 rounded-md bg-fg px-3 py-0.5 font-medium text-surface hover:bg-fg-2"
      >
        Reload
      </button>
      <button
        type="button"
        onClick={() => setOpen(false)}
        aria-label="Later"
        className="shrink-0 text-muted hover:text-fg"
      >
        ✕
      </button>
    </div>
  );
}

const kindStyle: Record<string, string> = {
  new: "bg-sky-500/15 text-sky-600 dark:text-sky-400",
  improved: "bg-emerald-500/15 text-ok",
  fixed: "bg-amber-500/15 text-warn",
};

/** C: a card in the bottom-right corner that says what reloading brings, from the changelog. */
export function Toast() {
  const [open, setOpen] = useState(true);
  if (!open) return null;
  return (
    <div className="fixed right-4 bottom-20 z-40 w-80 rounded-lg border border-line bg-surface p-4 text-sm shadow-lg sm:bottom-4">
      <div className="flex items-baseline justify-between">
        <span className="font-semibold">Mergepatrol was updated</span>
        <a href="/changelog" className="text-xs text-muted hover:text-fg">
          Changelog →
        </a>
      </div>
      <ul className="mt-3 space-y-2">
        {whatsNew.map((e) => (
          <li key={e.title} className="flex items-start gap-2">
            <span
              className={`mt-px shrink-0 rounded px-1.5 text-[10px] leading-4 font-semibold uppercase ${kindStyle[e.kind]}`}
            >
              {e.kind}
            </span>
            <span className="text-fg-2">{e.title}</span>
          </li>
        ))}
      </ul>
      <div className="mt-4 flex justify-end gap-2">
        <button
          type="button"
          onClick={() => setOpen(false)}
          className="rounded-md px-3 py-1 text-muted hover:text-fg"
        >
          Later
        </button>
        <button
          type="button"
          onClick={reload}
          className="rounded-md bg-fg px-3 py-1 font-medium text-surface hover:bg-fg-2"
        >
          Reload
        </button>
      </div>
    </div>
  );
}

/** The prototype's variant bar, dev builds only. */
export function PrototypeSwitcher({
  current,
  onChange,
}: {
  current: Variant;
  onChange: (v: Variant) => void;
}) {
  const step = (d: number) =>
    onChange(VARIANTS[(VARIANTS.indexOf(current) + d + VARIANTS.length) % VARIANTS.length]);
  useEffect(() => {
    const onKey = (e: KeyboardEvent) => {
      const t = e.target as HTMLElement;
      if (t.closest("input, textarea, select, [contenteditable]")) return;
      if (e.key === "ArrowLeft") step(-1);
      if (e.key === "ArrowRight") step(1);
    };
    addEventListener("keydown", onKey);
    return () => removeEventListener("keydown", onKey);
  });
  if (!import.meta.env.DEV) return null;
  return (
    <div className="fixed bottom-4 left-1/2 z-50 flex -translate-x-1/2 items-center gap-1 rounded-full bg-fuchsia-600 px-1 py-1 text-sm text-white shadow-xl">
      <button
        type="button"
        onClick={() => step(-1)}
        className="rounded-full px-3 py-1 hover:bg-white/20"
      >
        ←
      </button>
      <span className="px-2 font-medium tabular-nums">
        {current} ({NAMES[current]})
      </span>
      <button
        type="button"
        onClick={() => step(1)}
        className="rounded-full px-3 py-1 hover:bg-white/20"
      >
        →
      </button>
    </div>
  );
}
