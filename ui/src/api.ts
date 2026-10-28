import type { EventView } from "./api/EventView";
import type { LaneView } from "./api/LaneView";
import type { SetSecret } from "./api/SetSecret";
import type { SettingsView } from "./api/SettingsView";

async function get<T>(path: string): Promise<T> {
  const res = await fetch(path);
  if (!res.ok) throw new Error(`${path}: ${res.status}`);
  return res.json();
}

export const fetchLanes = () => get<LaneView[]>("/api/lanes");

export const fetchEvents = (repo: string, pr?: number) =>
  get<EventView[]>(`/api/events?repo=${encodeURIComponent(repo)}${pr ? `&pr=${pr}` : ""}`);

async function send<T>(method: string, path: string, body?: unknown): Promise<T> {
  const res = await fetch(path, {
    method,
    headers: body ? { "content-type": "application/json" } : undefined,
    body: body ? JSON.stringify(body) : undefined,
  });
  // Errors come back as plain text written for the user.
  if (!res.ok) throw new Error((await res.text()) || `${path}: ${res.status}`);
  return res.json();
}

export const fetchSettings = () => get<SettingsView>("/api/settings");

export const setAnthropicKey = (value: string) =>
  send<SettingsView>("PUT", "/api/settings/anthropic-api-key", { value } satisfies SetSecret);

export const deleteAnthropicKey = () =>
  send<SettingsView>("DELETE", "/api/settings/anthropic-api-key");
