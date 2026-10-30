import { useQuery } from "@tanstack/react-query";
import { useState } from "react";
import { fetchChangelog } from "./api";
import type { ChangelogKind } from "./api/ChangelogKind";
import { primary } from "./ui";

/** The hashed files a build of the dashboard loads, which every build names anew. Empty under
 * `vite dev`, which serves the source. */
function assets(doc: Document) {
  return [
    ...doc.querySelectorAll('script[src^="/assets/"], link[rel="stylesheet"][href^="/assets/"]'),
  ]
    .map((e) => e.getAttribute("src") ?? e.getAttribute("href"))
    .join(" ");
}

// Read at load, before anything adds tags of its own.
const running = assets(document);

/** The build the server serves now. */
async function deployed() {
  const res = await fetch("/", { cache: "no-store" });
  if (!res.ok) throw new Error(res.statusText);
  return assets(new DOMParser().parseFromString(await res.text(), "text/html"));
}

/** The changelog's pills, in its colors. */
const pills: Record<ChangelogKind, [string, string]> = {
  new: [
    "New",
    "border-green-200 bg-green-100 text-green-800 dark:border-green-900 dark:bg-green-950 dark:text-green-300",
  ],
  improved: [
    "Improved",
    "border-blue-200 bg-blue-100 text-blue-800 dark:border-blue-900 dark:bg-blue-950 dark:text-blue-300",
  ],
  fixed: [
    "Fixed",
    "border-amber-200 bg-amber-100 text-amber-800 dark:border-amber-900 dark:bg-amber-950 dark:text-amber-300",
  ],
};

const SHOWN = 3;

/** Once a newer dashboard is deployed than this page runs, offers to reload into it, with the
 * changelog entries that came since. Checks every few minutes and when the tab comes back. */
export function UpdateToast() {
  const latest = useQuery({
    queryKey: ["deployed"],
    queryFn: deployed,
    enabled: running !== "",
    refetchInterval: 5 * 60_000,
    retry: false,
  });
  const next = latest.data;
  const updated = !!next && next !== running;
  // The entries as of this page's load, and as of the newer build.
  const known = useQuery({
    queryKey: ["changelog", running],
    queryFn: fetchChangelog,
    enabled: running !== "",
    staleTime: Number.POSITIVE_INFINITY,
  });
  const now = useQuery({
    queryKey: ["changelog", next],
    queryFn: fetchChangelog,
    enabled: updated,
    staleTime: Number.POSITIVE_INFINITY,
  });
  const [later, setLater] = useState<string>();
  if (!updated || next === later) return null;
  const seen = new Set(known.data?.map((e) => e.slug));
  const news = known.data ? (now.data ?? []).filter((e) => !seen.has(e.slug)) : [];
  return (
    <div
      role="status"
      className="fixed right-4 bottom-4 z-40 w-80 max-w-[calc(100vw-2rem)] rounded-lg border border-line bg-surface p-4 text-sm shadow-lg"
    >
      <div className="flex items-baseline justify-between gap-3">
        <p className="font-semibold">Mergepatrol was updated</p>
        <a href="/changelog" className="shrink-0 text-xs text-muted hover:text-fg">
          Changelog →
        </a>
      </div>
      {news.length === 0 ? (
        <p className="mt-1 text-muted">Reload to get the newest version.</p>
      ) : (
        <ul className="mt-3 space-y-2">
          {news.slice(0, SHOWN).map((e) => (
            <li key={e.slug} className="flex items-start gap-2">
              <span
                className={`mt-px shrink-0 rounded-full border px-2 text-xs leading-[18px] font-semibold ${pills[e.kind][1]}`}
              >
                {pills[e.kind][0]}
              </span>
              <a href={`/changelog/${e.slug}`} className="text-fg-2 hover:text-fg hover:underline">
                {e.title}
              </a>
            </li>
          ))}
          {news.length > SHOWN && <li className="text-muted">and {news.length - SHOWN} more</li>}
        </ul>
      )}
      <div className="mt-4 flex justify-end gap-2">
        <button
          type="button"
          onClick={() => setLater(next)}
          className="h-9 rounded-md px-3 text-muted hover:text-fg"
        >
          Later
        </button>
        <button type="button" onClick={() => location.reload()} className={primary}>
          Reload
        </button>
      </div>
    </div>
  );
}
