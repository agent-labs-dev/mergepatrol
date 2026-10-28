import type { EventView } from "./api/EventView";
import type { LaneView } from "./api/LaneView";

async function get<T>(path: string): Promise<T> {
  const res = await fetch(path);
  if (!res.ok) throw new Error(`${path}: ${res.status}`);
  return res.json();
}

export const fetchLanes = () => get<LaneView[]>("/api/lanes");

export const fetchEvents = (repo: string, pr?: number) =>
  get<EventView[]>(`/api/events?repo=${encodeURIComponent(repo)}${pr ? `&pr=${pr}` : ""}`);
