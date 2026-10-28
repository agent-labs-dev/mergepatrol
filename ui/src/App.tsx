import { useQuery } from "@tanstack/react-query";
import { useState } from "react";
import { fetchEvents, fetchLanes } from "./api";
import type { EntryView } from "./api/EntryView";
import type { LaneView } from "./api/LaneView";

type Selected = { repo: string; pr: number };

export function App() {
  const [selected, setSelected] = useState<Selected | null>(null);
  const lanes = useQuery({ queryKey: ["lanes"], queryFn: fetchLanes, refetchInterval: 3000 });

  return (
    <main className="mx-auto max-w-5xl p-8">
      <h1 className="mb-6 text-2xl font-semibold">Mergepatrol</h1>
      {lanes.isError && <p className="text-red-400">Could not load the queue.</p>}
      {lanes.data?.length === 0 && <p className="text-neutral-400">No labeled PRs yet.</p>}
      <div className="grid gap-6 md:grid-cols-[1fr_1.2fr]">
        <div className="space-y-6">
          {lanes.data?.map((lane) => (
            <Lane key={`${lane.repo}:${lane.trunk}`} lane={lane} onSelect={setSelected} />
          ))}
        </div>
        {selected && <Events {...selected} />}
      </div>
    </main>
  );
}

function Lane({ lane, onSelect }: { lane: LaneView; onSelect: (s: Selected) => void }) {
  return (
    <section>
      <h2 className="mb-2 font-mono text-sm text-neutral-400">
        {lane.repo} → {lane.trunk}
      </h2>
      <ol className="divide-y divide-neutral-800 rounded border border-neutral-800">
        {lane.entries.length === 0 && <li className="p-3 text-neutral-500">Empty</li>}
        {lane.entries.map((e) => (
          <li key={e.number}>
            <button
              type="button"
              className="flex w-full items-center gap-3 p-3 text-left hover:bg-neutral-900"
              onClick={() => onSelect({ repo: lane.repo, pr: e.number })}
            >
              <span className="font-mono">#{e.number}</span>
              <State entry={e} />
              {e.priority && <span className="text-xs text-amber-400">priority</span>}
              {e.detail && <span className="truncate text-sm text-neutral-400">{e.detail}</span>}
            </button>
          </li>
        ))}
      </ol>
    </section>
  );
}

const stateStyle: Record<EntryView["state"], string> = {
  active: "bg-sky-900 text-sky-200",
  waiting: "bg-neutral-800 text-neutral-300",
  held: "bg-amber-900 text-amber-200",
};

function State({ entry }: { entry: EntryView }) {
  return (
    <span className={`rounded px-2 py-0.5 text-xs ${stateStyle[entry.state]}`}>{entry.state}</span>
  );
}

function Events({ repo, pr }: Selected) {
  const events = useQuery({
    queryKey: ["events", repo, pr],
    queryFn: () => fetchEvents(repo, pr),
    refetchInterval: 3000,
  });
  return (
    <section>
      <h2 className="mb-2 font-mono text-sm text-neutral-400">
        {repo}#{pr}
      </h2>
      <ol className="space-y-2">
        {events.data?.map((e, i) => (
          // Events have no ID in the API yet, and the list only grows at the top.
          // biome-ignore lint/suspicious/noArrayIndexKey: see above
          <li key={i} className="rounded border border-neutral-800 p-3 text-sm">
            <div className="flex justify-between text-xs text-neutral-500">
              <span>{e.kind}</span>
              <time>{e.at}</time>
            </div>
            <p className="mt-1 whitespace-pre-wrap">{e.message}</p>
          </li>
        ))}
      </ol>
    </section>
  );
}
