/*
 * PROTOTYPE, throw away. `pnpm prototype`: the dashboard against a stub API, so the PR detail
 * variants run without the server, signed in to a fake workspace with one lane and open PRs.
 * Open http://localhost:5173/acme/queue?pr=212
 */
import { mergeConfig, type Plugin } from "vite";
import base from "./vite.config.ts";

const ago = (min: number) => new Date(Date.now() - min * 60_000).toISOString();
const person = (login: string) => ({
  login,
  avatar_url: `https://github.com/${login}.png?size=40`,
});

const pull = (
  number: number,
  title: string,
  author: string,
  extra: Record<string, unknown> = {},
) => ({
  number,
  title,
  url: `https://github.com/acme/api/pull/${number}`,
  author: person(author),
  created_at: ago(number * 7),
  head_ref: `feat/${title
    .toLowerCase()
    .replace(/[^a-z]+/g, "-")
    .slice(0, 28)}`,
  fork: false,
  additions: 351,
  deletions: 4,
  changed_files: 7,
  review_decision: "approved",
  reviews: [{ reviewer: person("mpcode"), approved: true }],
  requested: [person("octocat")],
  checks: "passed",
  ...extra,
});

const lanes = [
  {
    repo: "acme/api",
    trunk: "main",
    batch: null,
    entries: [
      {
        ...pull(198, "fix: retry the webhook on 502", "mpcode"),
        priority: false,
        state: "active",
        detail: null,
        layers: [],
      },
      {
        ...pull(201, "feat: per-repo merge windows", "octocat"),
        priority: false,
        state: "waiting",
        detail: null,
        layers: [],
      },
    ],
  },
];

const open = [
  pull(212, "feat: redeliver failed webhooks, and prune the reads nothing makes", "jnsdls", {
    draft: false,
  }),
  pull(213, "fix: the batch survives a GraphQL rate limit", "mpcode", {
    draft: false,
    checks: "failed",
    review_decision: "review_required",
    reviews: [],
  }),
  pull(215, "feat: a dark theme for the tray", "octocat", {
    draft: false,
    checks: "pending",
    review_decision: "changes_requested",
    reviews: [{ reviewer: person("jnsdls"), approved: false }],
  }),
];

const routes: Record<string, unknown> = {
  "/api/session": {
    status: "signed_in",
    setup_unlocked: true,
    user: person("jnsdls"),
    app: { slug: "mergepatrol", html_url: "https://github.com", install_url: "https://github.com" },
    workspaces: [
      {
        installation_id: 1,
        account: "acme",
        account_type: "Organization",
        avatar_url: "https://github.com/github.png?size=40",
      },
    ],
    analytics: null,
    pricing: null,
  },
  "/api/w/1/lanes": lanes,
  "/api/w/1/repos/acme/api/open": open,
  "/api/w/1/events": [],
  "/api/w/1/settings": {
    api_keys: [{ provider: "anthropic", key: "sk-…" }],
    chatgpt: { login: null },
  },
};

export default mergeConfig(base, {
  plugins: [
    {
      name: "prototype-api",
      configureServer(server) {
        // No @types/node here: the request is typed by hand.
        server.middlewares.use((incoming, res, next) => {
          const req = incoming as unknown as { url?: string; method?: string };
          const path = req.url?.split("?")[0] ?? "";
          if (!path.startsWith("/api/")) return next();
          // Queue and dequeue succeed and change nothing.
          const body = req.method === "GET" ? routes[path] : null;
          if (body === undefined) {
            res.statusCode = 404;
            return res.end(`prototype: no stub for ${path}`);
          }
          res.setHeader("content-type", "application/json");
          res.end(JSON.stringify(body));
        });
      },
    } satisfies Plugin,
  ],
});
