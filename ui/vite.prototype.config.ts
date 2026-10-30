// PROTOTYPE, throw away: the dashboard against a stubbed API, signed in, for the
// update-available variants. `pnpm prototype`.
import { defineConfig, mergeConfig, type Plugin } from "vite";
import base from "./vite.config.ts";

const avatar = "https://avatars.githubusercontent.com/u/9919?v=4";
const stubs: [RegExp, unknown][] = [
  [
    /^\/api\/session$/,
    {
      status: "signed_in",
      setup_unlocked: true,
      user: { login: "octocat", avatar_url: "https://avatars.githubusercontent.com/u/583231?v=4" },
      app: { slug: "mergepatrol", html_url: "#", install_url: "#" },
      workspaces: [
        {
          installation_id: 1,
          account: "agent-labs-dev",
          account_type: "Organization",
          avatar_url: avatar,
        },
      ],
      analytics: null,
      pricing: null,
    },
  ],
  [/^\/api\/you$/, { authored: [], review_requested: [], merged: [] }],
  [
    /^\/api\/w\/\d+\/settings$/,
    {
      api_keys: [{ provider: "anthropic", key: { last4: "abcd" } }],
      chatgpt: { login: null, sign_in: null },
    },
  ],
  [
    /^\/api\/w\/\d+\/lanes$/,
    [{ repo: "agent-labs-dev/mergepatrol", trunk: "main", entries: [], batch: null }],
  ],
  [/^\/api\//, []],
];

const stubApi: Plugin = {
  name: "stub-api",
  configureServer(server) {
    server.middlewares.use((req, res, next) => {
      const path = (req.url ?? "").split("?")[0];
      const hit = stubs.find(([re]) => re.test(path));
      if (!hit) return next();
      res.setHeader("content-type", "application/json");
      res.end(JSON.stringify(hit[1]));
    });
  },
};

export default mergeConfig({ ...base, server: {} }, defineConfig({ plugins: [stubApi] }));
