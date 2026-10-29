import type { EventView } from "./api/EventView";
import type { LaneView } from "./api/LaneView";
import type { ManifestForm } from "./api/ManifestForm";
import type { ManifestRequest } from "./api/ManifestRequest";
import type { PullRequestRef } from "./api/PullRequestRef";
import type { RepoConfigView } from "./api/RepoConfigView";
import type { RepoSettingsView } from "./api/RepoSettingsView";
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

/** Workspace endpoints: `id` is the installation ID. */
const w = (id: number, path: string) => `/api/w/${id}${path}`;

export const fetchLanes = (id: number) => get<LaneView[]>(w(id, "/lanes"));

export const fetchEvents = (id: number, repo: string, pr?: number) =>
  get<EventView[]>(w(id, `/events?repo=${encodeURIComponent(repo)}${pr ? `&pr=${pr}` : ""}`));

export const fetchRepos = (id: number) => get<RepoView[]>(w(id, "/repos"));

export const setRepo = (id: number, repo: string, body: SetRepo) =>
  call("PUT", w(id, `/repos/${repo}`), body);

export const fetchRepoConfig = (id: number, repo: string) =>
  get<RepoConfigView>(w(id, `/repos/${repo}/config`));

/** Opens a PR that writes the settings to the repo's config file, or updates the open one. */
export const proposeRepoConfig = (id: number, repo: string, body: RepoSettingsView) =>
  send<PullRequestRef>("PUT", w(id, `/repos/${repo}/config`), body);

export const fetchSettings = (id: number) => get<SettingsView>(w(id, "/settings"));

export const setAnthropicKey = (id: number, value: string) =>
  send<SettingsView>("PUT", w(id, "/settings/anthropic-api-key"), { value } satisfies SetSecret);

export const deleteAnthropicKey = (id: number) =>
  send<SettingsView>("DELETE", w(id, "/settings/anthropic-api-key"));
