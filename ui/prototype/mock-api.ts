/*
 * PROTOTYPE, throw away. A fake `/api` for `pnpm prototype`, so the Queue page runs without the
 * server, GitHub or Postgres. State lives in memory and resets on restart.
 *
 * Question: what should "open (un-queued) PRs below the queue, with Queue / Priority queue" and
 * "Remove from queue on every queued PR" look like? See `src/QueuePrototype.tsx`.
 */
import type { Plugin } from "vite";

const W = 1;
const av = (login: string) => `https://github.com/${login}.png?size=40`;
const person = (login: string) => ({ login, avatar_url: av(login) });
const ago = (h: number) => new Date(Date.now() - h * 3600_000).toISOString();

type Pr = {
  number: number;
  title: string;
  author: string;
  head_ref: string;
  additions: number;
  deletions: number;
  changed_files: number;
  checks: "passed" | "failed" | "pending" | null;
  review_decision: "approved" | "review_required" | "changes_requested" | null;
  draft: boolean;
  hours: number;
};

const pr = (p: Partial<Pr> & Pick<Pr, "number" | "title" | "author">): Pr => ({
  head_ref: p.title
    .toLowerCase()
    .replace(/[^a-z0-9]+/g, "-")
    .slice(0, 32),
  additions: 40,
  deletions: 12,
  changed_files: 3,
  checks: "passed",
  review_decision: "approved",
  draft: false,
  hours: 5,
  ...p,
});

const repos: Record<
  string,
  { trunk: string; prs: Pr[]; queue: { n: number; priority: boolean }[] }
> = {
  "agent-labs-dev/mergepatrol": {
    trunk: "main",
    prs: [
      pr({
        number: 101,
        title: "feat: open PRs on the Queue page",
        author: "jnsdls",
        additions: 212,
        deletions: 40,
      }),
      pr({
        number: 102,
        title: "fix: rebase loses the co-author trailer",
        author: "octocat",
        checks: "pending",
      }),
      pr({
        number: 104,
        title: "chore: bump tailwind to 4.3",
        author: "dependabot[bot]",
        additions: 18,
        deletions: 18,
        changed_files: 2,
      }),
      pr({
        number: 105,
        title: "feat: batch size per repo",
        author: "hubot",
        review_decision: "review_required",
      }),
      pr({
        number: 106,
        title: "refactor: split the queue loop",
        author: "monalisa",
        additions: 640,
        deletions: 590,
        changed_files: 14,
      }),
      pr({
        number: 107,
        title: "docs: self-hosting on Fly",
        author: "octocat",
        checks: null,
        review_decision: null,
        hours: 30,
      }),
      pr({
        number: 108,
        title: "fix: tray icon blurry on retina",
        author: "jnsdls",
        checks: "failed",
      }),
      pr({
        number: 109,
        title: "wip: GitLab support",
        author: "monalisa",
        draft: true,
        review_decision: "review_required",
        checks: "pending",
        hours: 72,
        additions: 1840,
        deletions: 22,
        changed_files: 31,
      }),
      pr({
        number: 110,
        title: "feat: Slack notifications on eject",
        author: "hubot",
        review_decision: "changes_requested",
        hours: 50,
      }),
      pr({
        number: 111,
        title: "fix: History chart off by one day",
        author: "octocat",
        hours: 2,
        additions: 6,
        deletions: 3,
        changed_files: 1,
      }),
    ],
    queue: [
      { n: 102, priority: true },
      { n: 101, priority: false },
      { n: 104, priority: false },
      { n: 105, priority: false },
    ],
  },
  "agent-labs-dev/skills": {
    trunk: "main",
    prs: [
      pr({ number: 31, title: "feat: prototype skill UI branch", author: "jnsdls" }),
      pr({
        number: 33,
        title: "fix: typo in grill-me",
        author: "octocat",
        additions: 1,
        deletions: 1,
        changed_files: 1,
      }),
    ],
    queue: [],
  },
};

const entry = (repo: string, q: { n: number; priority: boolean }, i: number) => {
  const p = repos[repo].prs.find((x) => x.number === q.n) as Pr;
  const held = p.review_decision === "review_required" || p.review_decision === "changes_requested";
  return {
    number: p.number,
    priority: q.priority,
    state: held ? "held" : i < 2 ? "active" : "waiting",
    detail: held ? "Waits for a required review." : null,
    ...view(repo, p),
  };
};

const view = (repo: string, p: Pr) => ({
  title: p.title,
  url: `https://github.com/${repo}/pull/${p.number}`,
  author: person(p.author),
  created_at: ago(p.hours),
  head_ref: p.head_ref,
  fork: false,
  additions: p.additions,
  deletions: p.deletions,
  changed_files: p.changed_files,
  review_decision: p.review_decision,
  reviews:
    p.review_decision === "approved" ? [{ reviewer: person("monalisa"), approved: true }] : [],
  requested: p.review_decision === "review_required" ? [person("monalisa")] : [],
  checks: p.checks,
  layers: [],
});

const lanes = () =>
  Object.entries(repos).map(([repo, r]) => {
    // Priority first, then queue order, like the real queue.
    const order = [...r.queue.filter((q) => q.priority), ...r.queue.filter((q) => !q.priority)];
    const entries = order.map((q, i) => entry(repo, q, i));
    const active = entries.filter((e) => e.state === "active");
    return {
      repo,
      trunk: r.trunk,
      batch: active.length > 1 ? 120 : null,
      entries: [...active, ...entries.filter((e) => e.state !== "active")],
    };
  });

const open = (repo: string) =>
  repos[repo].prs
    .filter((p) => !repos[repo].queue.some((q) => q.n === p.number))
    .map((p) => ({ number: p.number, draft: p.draft, ...view(repo, p) }));

const json = (res: import("node:http").ServerResponse, body: unknown) => {
  res.setHeader("content-type", "application/json");
  res.end(JSON.stringify(body));
};

export const mockApi = (): Plugin => ({
  name: "prototype-mock-api",
  configureServer(server) {
    server.middlewares.use((req, res, next) => {
      if (!req.url?.startsWith("/api")) return next();
      const url = new URL(req.url, "http://x");
      const path = url.pathname;
      const repo = url.searchParams.get("repo") ?? "";
      const n = Number(url.searchParams.get("pr"));
      if (path === "/api/session")
        return json(res, {
          status: "signed_in",
          setup_unlocked: true,
          user: person("jnsdls"),
          app: {
            slug: "mergepatrol",
            html_url: "https://github.com/apps/mergepatrol",
            install_url: "#",
          },
          workspaces: [
            {
              installation_id: W,
              account: "agent-labs-dev",
              account_type: "Organization",
              avatar_url: av("agent-labs-dev"),
            },
          ],
          analytics: null,
          pricing: null,
        });
      if (path === `/api/w/${W}/settings`)
        return json(res, {
          api_keys: [{ provider: "anthropic", key: "sk-…" }],
          chatgpt: { login: null },
        });
      if (path === `/api/w/${W}/lanes`) return json(res, lanes());
      if (path === `/api/w/${W}/open`) return json(res, open(repo));
      if (path === `/api/w/${W}/events`)
        return json(
          res,
          repos[repo]?.queue.some((q) => q.n === n)
            ? [{ repo, pr: n, at: ago(0.2), kind: "queued", message: `Queued #${n}`, spend: null }]
            : [],
        );
      if (path === `/api/w/${W}/queue` && req.method !== "GET") {
        const q = repos[repo]?.queue;
        if (!q) return json(res, null);
        const at = q.findIndex((x) => x.n === n);
        if (req.method === "DELETE") {
          if (at >= 0) q.splice(at, 1);
        } else {
          const priority = url.searchParams.get("priority") === "true";
          if (at >= 0) q[at].priority = priority;
          else q.push({ n, priority });
        }
        return json(res, null);
      }
      return json(res, null);
    });
  },
});
