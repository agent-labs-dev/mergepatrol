import type { PostHog } from "posthog-js";
import { useEffect } from "react";
import type { AnalyticsView } from "./api/AnalyticsView";
import type { SessionView } from "./api/SessionView";
import type { WorkspaceView } from "./api/WorkspaceView";

/*
 * Product analytics in PostHog, when the server turns them on (the hosted Mergepatrol). A person
 * is a GitHub login, and a `workspace` group is an installation of the App: the same keys the
 * server sends its events with. posthog-js loads only then, so a self-hosted dashboard never
 * downloads it.
 */

let client: Promise<PostHog> | undefined;

function start(config: AnalyticsView) {
  client ??= import("posthog-js").then(({ default: posthog }) => {
    posthog.init(config.key, {
      api_host: config.host,
      defaults: "2026-08-30",
      person_profiles: "identified_only",
    });
    return posthog;
  });
  return client;
}

export function useAnalytics(session: SessionView | undefined) {
  const config = session?.analytics;
  const login = session?.user?.login;
  const avatar_url = session?.user?.avatar_url;
  useEffect(() => {
    if (!config) return;
    start(config).then((posthog) => {
      if (login) posthog.identify(login, { login, avatar_url });
    });
  }, [config, login, avatar_url]);
}

/** Attributes what the user does next to the workspace in view. */
export function useWorkspaceAnalytics(session: SessionView, workspace: WorkspaceView | undefined) {
  const config = session.analytics;
  useEffect(() => {
    if (!config || !workspace) return;
    start(config).then((posthog) =>
      posthog.group("workspace", String(workspace.installation_id), {
        name: workspace.account,
        account_type: workspace.account_type,
      }),
    );
  }, [config, workspace]);
}

/** An event for the signed-in user, in the workspace in view. Nothing when analytics are off. */
export function capture(event: string, properties?: Record<string, unknown>) {
  client?.then((posthog) => posthog.capture(event, properties));
}

export function resetAnalytics() {
  client?.then((posthog) => posthog.reset());
}
