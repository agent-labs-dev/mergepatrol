/*
 * Pre-signup analytics for the server-rendered blog pages, which do not load the dashboard bundle.
 *
 * Same PostHog project, the same `person_profiles: "identified_only"` setting and the same
 * persistence as the dashboard (ui/src/analytics.ts), so an anonymous visitor who reads an article
 * and later signs in on mergepatrol.ai is identified as the same person: the article stays attached
 * to their timeline. `window.__mp` is written into the page by crates/web/src/blog.rs, and is
 * absent when analytics are off (self-hosted), so nothing is sent and no script is loaded.
 */
(() => {
  var config = window.__mp;
  if (!config || !config.key || !config.host) return;

  var host = config.host.replace(/\/+$/, "");
  var script = document.createElement("script");
  script.async = true;
  script.src = host + "/static/array.js";
  script.onload = () => {
    var posthog = window.posthog;
    if (!posthog) return;
    posthog.init(config.key, {
      api_host: config.host,
      defaults: "2026-08-30",
      person_profiles: "identified_only",
    });
    posthog.capture("blog_viewed", {
      surface: config.surface,
      slug: config.slug,
      path: location.pathname,
      referrer: document.referrer || undefined,
    });
    document.addEventListener("click", (event) => {
      var target = event.target;
      var link = target && target.closest ? target.closest('a[href="/auth/login"]') : null;
      if (!link) return;
      posthog.capture("cta_clicked", {
        cta: "sign_in_github",
        location: "blog",
        surface: config.surface,
        slug: config.slug,
        path: location.pathname,
      });
    });
  };
  document.head.appendChild(script);
})();
