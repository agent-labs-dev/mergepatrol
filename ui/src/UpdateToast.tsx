import { useQuery } from "@tanstack/react-query";
import { useState } from "react";
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

/** Once a newer dashboard is deployed than this page runs, offers to reload into it. Checks every
 * few minutes and when the tab comes back. */
export function UpdateToast() {
  const latest = useQuery({
    queryKey: ["deployed"],
    queryFn: deployed,
    enabled: running !== "",
    refetchInterval: 5 * 60_000,
    retry: false,
  });
  const [later, setLater] = useState<string>();
  const next = latest.data;
  if (!next || next === running || next === later) return null;
  return (
    <div
      role="status"
      className="fixed right-4 bottom-4 z-40 w-80 max-w-[calc(100vw-2rem)] rounded-lg border border-line bg-surface p-4 text-sm shadow-lg"
    >
      <p className="font-semibold">Mergepatrol was updated</p>
      <p className="mt-1 text-muted">Reload to get the newest version.</p>
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
