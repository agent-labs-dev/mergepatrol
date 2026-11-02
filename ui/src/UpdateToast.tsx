import { useQuery } from "@tanstack/react-query";
import { useEffect, useState } from "react";
import { fetchChangelog } from "./api";
import type { ChangelogEntryView } from "./api/ChangelogEntryView";
import type { ChangelogKind } from "./api/ChangelogKind";
import { toasts } from "./Toasts";

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
  useEffect(() => {
    if (!updated || next === later) return;
    const seen = new Set(known.data?.map((e) => e.slug));
    const news = known.data ? (now.data ?? []).filter((e) => !seen.has(e.slug)) : [];
    // The same id each time, so the entries fill in the toast already up.
    toasts.add({
      id: "update",
      type: "update",
      title: "MergeLoop was updated",
      description: <News news={news} />,
      timeout: 0,
      onClose: () => setLater(next),
      data: {
        actions: [
          { label: "Later", onClick: () => toasts.close("update") },
          { label: "Reload", onClick: () => location.reload(), primary: true },
        ],
      },
    });
  }, [updated, next, later, known.data, now.data]);
  return null;
}

/** The changelog entries since this page loaded, or a nudge to reload when there are none. */
function News({ news }: { news: ChangelogEntryView[] }) {
  if (news.length === 0) return <>Reload to get the newest version.</>;
  return (
    <>
      <ul className="mt-2 space-y-2">
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
        {news.length > SHOWN && <li>and {news.length - SHOWN} more</li>}
      </ul>
      <a href="/changelog" className="mt-2 inline-block text-xs hover:text-fg">
        Full changelog →
      </a>
    </>
  );
}
