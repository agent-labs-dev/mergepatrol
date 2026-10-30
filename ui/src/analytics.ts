import type { PostHog } from "posthog-js";
import { useEffect, useRef } from "react";
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

/*
 * The visitor who has not signed in yet. These events go out on the same client and the same
 * PostHog project, with no `identify`, so posthog-js keeps the anonymous distinct_id it made at
 * first load (a cookie and localStorage on the same origin). When the visitor signs in later,
 * `useAnalytics` identifies that same browser, so the article and the call-to-action they came
 * through stay on one person's timeline.
 */

/** The article a visitor arrived from, when the referrer is one of our own blog pages. */
function fromBlogSlug(referrer: string): string | undefined {
  try {
    const url = new URL(referrer);
    if (url.origin !== location.origin) return undefined;
    return url.pathname.match(/^\/blog\/([a-z0-9-]+)\/?$/)?.[1];
  } catch {
    return undefined;
  }
}

/** What the visitor brought with them: the page, where they came from, and any campaign in the URL. */
function visitorProperties(): Record<string, unknown> {
  const params = new URLSearchParams(location.search);
  const properties: Record<string, unknown> = {
    path: location.pathname,
    referrer: document.referrer || undefined,
    from_blog_slug: fromBlogSlug(document.referrer),
  };
  for (const name of ["utm_source", "utm_medium", "utm_campaign"]) {
    const value = params.get(name);
    if (value) properties[name] = value;
  }
  return properties;
}

/** The landing page, once per load, for a visitor with no sign-in. */
export function useLandingViewed(config: AnalyticsView | null | undefined) {
  const sent = useRef(false);
  useEffect(() => {
    if (!config || sent.current) return;
    sent.current = true;
    start(config).then((posthog) =>
      posthog.capture("landing_viewed", { ...visitorProperties(), surface: "landing" }),
    );
  }, [config]);
}

/** A click on a call to action, while the visitor is still anonymous. */
export function trackCta(
  config: AnalyticsView | null | undefined,
  cta: "sign_in_github" | "install_app" | "see_pricing",
  place: "nav" | "hero" | "footer" | "pricing" | "install",
) {
  if (!config) return;
  start(config).then((posthog) =>
    posthog.capture("cta_clicked", { ...visitorProperties(), cta, location: place }),
  );
}
