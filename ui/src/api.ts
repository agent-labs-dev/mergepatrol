import type { BillingView } from "./api/BillingView";
import type { EntryView } from "./api/EntryView";
import type { EventView } from "./api/EventView";
import type { ExitsPage } from "./api/ExitsPage";
import type { FixView } from "./api/FixView";
import type { HistoryStatsView } from "./api/HistoryStatsView";
import type { LaneView } from "./api/LaneView";
import type { ManifestForm } from "./api/ManifestForm";
import type { ManifestRequest } from "./api/ManifestRequest";
import type { OutcomeView } from "./api/OutcomeView";
import type { PullRequestRef } from "./api/PullRequestRef";
import type { RedirectView } from "./api/RedirectView";
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

/** The changes of the fix agent's commit `sha`. */
export const fetchFix = (id: number, repo: string, sha: string) =>
  get<FixView>(w(id, `/fix?repo=${encodeURIComponent(repo)}&sha=${sha}`));

/** The query string of the defined values. */
const query = (params: Record<string, string | number | undefined>) =>
  new URLSearchParams(
    Object.entries(params).flatMap(([k, v]) => (v === undefined ? [] : [[k, String(v)]])),
  ).toString();

/** Without `repo`, every repo's; without `author`, everyone's; without `outcome`, every
 * outcome's. */
export const fetchExits = (
  id: number,
  q: { repo?: string; author?: string; days: number; outcome?: OutcomeView; before?: number },
) => get<ExitsPage>(w(id, `/history?${query(q)}`));

export const fetchHistoryStats = (
  id: number,
  q: { repo?: string; author?: string; days: number },
) => get<HistoryStatsView>(w(id, `/history/stats?${query(q)}`));

export const fetchRepos = (id: number) => get<RepoView[]>(w(id, "/repos"));

export const setRepo = (id: number, repo: string, body: SetRepo) =>
  call("PUT", w(id, `/repos/${repo}`), body);

export const fetchRepoConfig = (id: number, repo: string) =>
  get<RepoConfigView>(w(id, `/repos/${repo}/config`));

/** Opens a PR that writes the settings to the repo's config file, or updates the open one. */
export const proposeRepoConfig = (id: number, repo: string, body: RepoSettingsView) =>
  send<PullRequestRef>("PUT", w(id, `/repos/${repo}/config`), body);

export const fetchSettings = (id: number) => get<SettingsView>(w(id, "/settings"));

export const setApiKey = (id: number, provider: string, value: string) =>
  send<SettingsView>("PUT", w(id, `/settings/api-keys/${provider}`), {
    value,
  } satisfies SetSecret);

export const deleteApiKey = (id: number, provider: string) =>
  send<SettingsView>("DELETE", w(id, `/settings/api-keys/${provider}`));

/** `null` without billing, as when self-hosted. */
export const fetchBilling = (id: number) => get<BillingView | null>(w(id, "/billing"));

/** A Stripe Checkout page that subscribes the workspace. */
export const startCheckout = (id: number) => send<RedirectView>("POST", w(id, "/billing/checkout"));

/** Stripe's customer portal: the card, invoices, and cancelling. */
export const openPortal = (id: number) => send<RedirectView>("POST", w(id, "/billing/portal"));

/** Frees a developer's seat: they take one again with their next PR on a private repo. */
export const freeSeat = (id: number, login: string) =>
  send<BillingView>("DELETE", w(id, `/billing/seats/${encodeURIComponent(login)}`));

/** Starts a sign-in to the workspace's ChatGPT plan: the user enters the code it returns. */
export const startChatGptLogin = (id: number) =>
  send<SettingsView>("POST", w(id, "/settings/chatgpt"));

/** Asks whether the user has entered the code yet. */
export const pollChatGptLogin = (id: number) =>
  send<SettingsView>("POST", w(id, "/settings/chatgpt/poll"));

/** Signs out, or cancels a sign-in. */
export const deleteChatGptLogin = (id: number) =>
  send<SettingsView>("DELETE", w(id, "/settings/chatgpt"));

// PROTOTYPE, throw away: endpoints only `prototype/mock-api.ts` serves. See `QueuePrototype.tsx`.

/** A repo's open PRs that are not in its queue. */
export const fetchOpenPulls = (id: number, repo: string) =>
  get<OpenPull[]>(w(id, `/open?repo=${encodeURIComponent(repo)}`));

export type OpenPull = Omit<EntryView, "priority" | "state" | "detail"> & { draft: boolean };

/** Queues the PR, or changes its priority when it is queued already. */
export const queuePull = (id: number, repo: string, pr: number, priority: boolean) =>
  call("POST", w(id, `/queue?repo=${encodeURIComponent(repo)}&pr=${pr}&priority=${priority}`));

export const dequeuePull = (id: number, repo: string, pr: number) =>
  call("DELETE", w(id, `/queue?repo=${encodeURIComponent(repo)}&pr=${pr}`));
