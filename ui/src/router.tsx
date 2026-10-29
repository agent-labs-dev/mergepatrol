import {
  createRootRoute,
  createRoute,
  createRouter,
  Navigate,
  redirect,
} from "@tanstack/react-router";
import { App } from "./App";
import type { OutcomeView } from "./api/OutcomeView";
import { History, OUTCOMES, WINDOWS, type Window } from "./History";
import { HistoryPrototype } from "./HistoryPrototype";
import { Queue } from "./Queue";
import { Repos } from "./Repos";
import { Settings } from "./Settings";
import { useWorkspace } from "./workspace";

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

const queue = createRoute({
  getParentRoute: () => workspace,
  path: "queue",
  /** The lane in view, as `owner/repo:trunk`, and the PR in it. Both default to the first. */
  validateSearch: (search): { lane?: string; pr?: number } => ({
    lane: optional(search.lane, "string"),
    pr: optional(search.pr, "number"),
  }),
  component: () => <Queue workspace={useWorkspace().workspace} />,
});

// PROTOTYPE: the developer-stats variants in development.
const HistoryPage = import.meta.env.DEV ? HistoryPrototype : History;

const history = createRoute({
  getParentRoute: () => workspace,
  path: "history",
  /** The repo in view, the window in days, and the outcome listed. Every repo, 30 days, and
   * every outcome by default. */
  validateSearch: (
    search,
  ): {
    repo?: string;
    days?: Window;
    outcome?: OutcomeView;
    // PROTOTYPE: developer stats.
    variant?: string;
    author?: string;
    view?: string;
    by?: string;
    sort?: string;
  } => ({
    repo: optional(search.repo, "string"),
    days: oneOf(WINDOWS, search.days),
    outcome: oneOf(OUTCOMES, search.outcome),
    variant: optional(search.variant, "string"),
    author: optional(search.author, "string"),
    view: optional(search.view, "string"),
    by: optional(search.by, "string"),
    sort: optional(search.sort, "string"),
  }),
  component: () => <HistoryPage workspace={useWorkspace().workspace} />,
});

const repos = createRoute({
  getParentRoute: () => workspace,
  path: "repos",
  /** The repo whose settings are open. */
  validateSearch: (search): { repo?: string } => ({ repo: optional(search.repo, "string") }),
  component: () => <Repos {...useWorkspace()} />,
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
    workspace.addChildren([workspaceHome, queue, history, repos, settings]),
  ]),
});

declare module "@tanstack/react-router" {
  interface Register {
    router: typeof router;
  }
}
