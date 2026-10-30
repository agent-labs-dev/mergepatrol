import {
  createRootRoute,
  createRoute,
  createRouter,
  Navigate,
  redirect,
} from "@tanstack/react-router";
import { App } from "./App";
import type { OutcomeView } from "./api/OutcomeView";
import { History, LISTS, type List, OUTCOMES, WINDOWS, type Window } from "./History";
import { type OpenFilter, REVIEWS, STATUSES } from "./OpenFilters";
import { Permalink } from "./Permalink";
import { Queue } from "./Queue";
import { Repos } from "./Repos";
import { Settings } from "./Settings";
import { useWorkspace } from "./workspace";
import { FOLDERS, type Folder, You } from "./You";

/*
 * The dashboard's pages, one path each, so a reload lands where you were and PostHog sees each
 * move as a pageview. The setup and sign-in screens have no paths: the session picks them.
 */

const optional = <T,>(value: unknown, type: "string" | "number") =>
  typeof value === type ? (value as T) : undefined;

const oneOf = <T,>(options: readonly T[], value: unknown) =>
  options.includes(value as T) ? (value as T) : undefined;

const root = createRootRoute({
  component: App,
  // A path that names no page goes home, which picks a workspace.
  notFoundComponent: () => <Navigate to="/" replace />,
});

// The dashboard redirects from here to the workspace it remembers.
const home = createRoute({ getParentRoute: () => root, path: "/" });

// The menu bar app's sign-in lands here. `App` shows it whatever the session is.
const tray = createRoute({ getParentRoute: () => root, path: "tray" });

/** A workspace, by its GitHub account. */
const workspace = createRoute({ getParentRoute: () => root, path: "$account" });

const workspaceHome = createRoute({
  getParentRoute: () => workspace,
  path: "/",
  beforeLoad: ({ params }) => {
    throw redirect({ to: "/$account/queue", params, replace: true });
  },
});

const you = createRoute({
  getParentRoute: () => workspace,
  path: "you",
  /** The folder in view, Needs you by default, and the PR in it as `owner/repo#n`. */
  validateSearch: (search): { folder?: Folder; pr?: string } => ({
    folder: oneOf(FOLDERS, search.folder),
    pr: optional(search.pr, "string"),
  }),
  component: You,
});

const queue = createRoute({
  getParentRoute: () => workspace,
  path: "queue",
  /** The lane in view, as `owner/repo:trunk`, and the PR in it, both the first by default,
   * whether the agent's review of the PR is being answered, and the filters on its open PRs. */
  validateSearch: (search): { lane?: string; pr?: number; answer?: boolean } & OpenFilter => ({
    lane: optional(search.lane, "string"),
    pr: optional(search.pr, "number"),
    // Answering the agent's review of the PR.
    answer: search.answer === true || undefined,
    author: optional(search.author, "string"),
    status: oneOf(STATUSES, search.status),
    review: oneOf(REVIEWS, search.review),
    drafts: search.drafts === true || undefined,
    q: optional(search.q, "string"),
  }),
  component: () => <Queue workspace={useWorkspace().workspace} />,
});

const history = createRoute({
  getParentRoute: () => workspace,
  path: "history",
  /** What the side lists, the repo and the developer in view, the window in days, and the
   * outcome listed. Repos, every repo and developer, 30 days, and every outcome by default.
   * `pr` lists only that PR of `repo`, from any time. */
  validateSearch: (
    search,
  ): {
    list?: List;
    repo?: string;
    author?: string;
    days?: Window;
    outcome?: OutcomeView;
    pr?: number;
  } => ({
    list: oneOf(LISTS, search.list),
    repo: optional(search.repo, "string"),
    author: optional(search.author, "string"),
    days: oneOf(WINDOWS, search.days),
    outcome: oneOf(OUTCOMES, search.outcome),
    pr: optional(search.pr, "number"),
  }),
  component: () => <History workspace={useWorkspace().workspace} />,
});

const repos = createRoute({
  getParentRoute: () => workspace,
  path: "repos",
  /** The repo whose settings are open. */
  validateSearch: (search): { repo?: string } => ({ repo: optional(search.repo, "string") }),
  component: () => <Repos {...useWorkspace()} />,
});

/** A PR's permalink, which the API hands out: it opens wherever the PR is. */
const pull = createRoute({
  getParentRoute: () => workspace,
  path: "$repo/pull/$number",
  component: () => <Permalink workspace={useWorkspace().workspace} />,
});

const settings = createRoute({
  getParentRoute: () => workspace,
  path: "settings",
  component: () => <Settings workspace={useWorkspace().workspace} />,
});

export const router = createRouter({
  routeTree: root.addChildren([
    home,
    tray,
    workspace.addChildren([workspaceHome, you, queue, history, repos, settings, pull]),
  ]),
});

declare module "@tanstack/react-router" {
  interface Register {
    router: typeof router;
  }
}
