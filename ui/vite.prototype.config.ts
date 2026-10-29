// PROTOTYPE, delete me: the dashboard against an in-memory API, for the no-LLM-key banner.
// `pnpm prototype`, then open http://localhost:5174/acme/queue?variant=A
import type { Plugin } from "vite";
import { defineConfig, mergeConfig } from "vite";
import base from "./vite.config.ts";

const now = Date.now();
const ago = (mins: number) => new Date(now - mins * 60_000).toISOString();
const person = (login: string) => ({ login, avatar_url: `https://github.com/${login}.png` });

let key: { hint: string; updated_at: string } | null = null;

const entry = (number: number, state: string, title: string, detail: string | null = null) => ({
  number,
  priority: false,
  state,
  detail,
  title,
  url: `https://github.com/acme/web/pull/${number}`,
  author: person("octocat"),
  created_at: ago(number * 7),
  head_ref: `feat/${number}`,
  additions: 120,
  deletions: 30,
  changed_files: 6,
  review_decision: "approved",
  reviews: [{ reviewer: person("hubot"), approved: true }],
  requested: [],
  checks: state === "held" ? "failed" : "pending",
  layers: [],
});

const routes: Record<string, (method: string, body: string) => unknown> = {
  "/api/session": () => ({
    status: "signed_in",
    setup_unlocked: true,
    user: person("octocat"),
    app: { slug: "mergepatrol", html_url: "#", install_url: "#" },
    workspaces: [
      {
        installation_id: 1,
        account: "acme",
        account_type: "Organization",
        avatar_url: "https://github.com/github.png",
      },
    ],
    analytics: null,
  }),
  "/api/w/1/lanes": () => [
    {
      repo: "acme/web",
      trunk: "main",
      batch: 412,
      entries: [
        entry(401, "active", "Add billing page"),
        entry(398, "waiting", "Bump React to 19.3"),
        entry(
          395,
          "held",
          "Refactor auth middleware",
          "Rebase has conflicts. No Anthropic API key: add one in the dashboard settings",
        ),
      ],
    },
  ],
  "/api/w/1/events": () => [],
  "/api/w/1/repos": () => [
    {
      repo: "acme/web",
      private: true,
      on: true,
      enabled: true,
      has_config: true,
      config_error: null,
    },
  ],
  "/api/w/1/settings": () => ({ anthropic_api_key: key }),
  "/api/w/1/settings/anthropic-api-key": (method, body) => {
    if (method === "PUT") {
      const value: string = JSON.parse(body).value;
      key = { hint: value.slice(-4), updated_at: new Date().toISOString() };
    } else key = null;
    return { anthropic_api_key: key };
  },
};

const mockApi: Plugin = {
  name: "prototype-mock-api",
  configureServer(server) {
    server.middlewares.use((req, res, next) => {
      const path = req.url?.split("?")[0] ?? "";
      const route = routes[path];
      if (!route) return path.startsWith("/api") ? res.writeHead(404).end("not mocked") : next();
      let body = "";
      req.on("data", (c) => {
        body += c;
      });
      req.on("end", () => {
        res.setHeader("content-type", "application/json");
        res.end(JSON.stringify(route(req.method ?? "GET", body)));
      });
    });
  },
};

export default mergeConfig(base, defineConfig({ plugins: [mockApi] }));
