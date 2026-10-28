import type { EventView } from "./api/EventView";
import type { LaneView } from "./api/LaneView";
import type { ManifestForm } from "./api/ManifestForm";
import type { ManifestRequest } from "./api/ManifestRequest";
import type { RepoView } from "./api/RepoView";
import type { SessionView } from "./api/SessionView";
import type { SetRepo } from "./api/SetRepo";
import type { SetSecret } from "./api/SetSecret";
import type { SettingsView } from "./api/SettingsView";
import type { Unlock } from "./api/Unlock";

async function request(method: string, path: string, body?: unknown): Promise<Response> {
  const res = await fetch(path, {
    method,
    headers: body ? { "content-type": "application/json" } : undefined,
    body: body ? JSON.stringify(body) : undefined,
  });
  // Errors come back as plain text written for the user.
  if (!res.ok) throw new Error((await res.text()) || `${path}: ${res.status}`);
  return res;
}

const get = async <T>(path: string): Promise<T> => (await request("GET", path)).json();
const send = async <T>(method: string, path: string, body?: unknown): Promise<T> =>
  (await request(method, path, body)).json();
const call = async (method: string, path: string, body?: unknown): Promise<void> => {
  await request(method, path, body);
};

export const fetchSession = () => get<SessionView>("/api/session");

export const unlock = (token: string) =>
  call("POST", "/api/setup/unlock", { token } satisfies Unlock);

export const startManifest = (body: ManifestRequest) =>
  send<ManifestForm>("POST", "/api/setup/manifest", body);

export const logout = () => call("POST", "/auth/logout");

export const fetchLanes = () => get<LaneView[]>("/api/lanes");

export const fetchEvents = (repo: string, pr?: number) =>
  get<EventView[]>(`/api/events?repo=${encodeURIComponent(repo)}${pr ? `&pr=${pr}` : ""}`);

export const fetchRepos = () => get<RepoView[]>("/api/repos");

export const setRepo = (repo: string, body: SetRepo) => call("PUT", `/api/repos/${repo}`, body);

export const fetchSettings = () => get<SettingsView>("/api/settings");

export const setAnthropicKey = (value: string) =>
  send<SettingsView>("PUT", "/api/settings/anthropic-api-key", { value } satisfies SetSecret);

export const deleteAnthropicKey = () =>
  send<SettingsView>("DELETE", "/api/settings/anthropic-api-key");
