// PROTOTYPE, throwaway: a fake API for `pnpm prototype:stats`, so the developer-stats variants
// of /history run without a backend. Same seed, same data. Adds the proposed `author` filter and
// `developers` list to /history and /history/stats.
import type { Plugin } from "vite";

const DAY = 86_400_000;
const STEPS = ["waiting", "held", "rebasing", "checks", "fixing"] as const;
const REPOS = ["acme/api", "acme/web", "acme/infra", "acme/mobile", "acme/docs"];
const DEVS = [
  "octocat",
  "hubot",
  "monalisa",
  "defunkt",
  "mojombo",
  "pjhyett",
  "wycats",
  "tenderlove",
  "dependabot[bot]",
  "renovate[bot]",
];
const MODELS = [
  ["claude-opus-5-5", "high"],
  ["claude-sonnet-5-5", "medium"],
  ["gpt-5.5-codex", "high"],
] as const;

let seed = 7;
const rand = () => {
  seed = (seed * 16807) % 2147483647;
  return seed / 2147483647;
};
const pick = <T>(xs: readonly T[], skew = 1) => xs[Math.floor(rand() ** skew * xs.length)];

type Llm = {
  model: string;
  effort: string;
  kind: "rebase" | "fix";
  cost: number | null;
  failed: boolean;
  turns: number;
};
type Exit = {
  id: number;
  repo: string;
  trunk: string;
  number: number;
  outcome: "merged" | "ejected" | "removed";
  detail: string;
  title: string;
  url: string;
  author: { login: string; avatar_url: string } | null;
  fork: boolean;
  enqueued_at: string | null;
  steps: { step: string; secs: number }[];
  at: string;
  llm: Llm[];
};

// Each developer has a personality: how often they get ejected and how slow their PRs are.
const persona = Object.fromEntries(
  DEVS.map((d) => [
    d,
    { eject: d.endsWith("[bot]") ? 0.04 : 0.05 + rand() * 0.3, slow: 0.5 + rand() * 2 },
  ]),
);

const TITLES = [
  "fix: retry flaky webhook delivery",
  "feat: add pagination to the audit log",
  "chore: bump deps",
  "refactor: split the billing module",
  "fix: race in the session refresh",
  "feat: dark mode for settings",
  "docs: explain the merge queue",
  "perf: cache the repo config",
  "fix: null author on fork PRs",
  "feat: export history as CSV",
];
const REASONS = [
  "checks failed: test (ubuntu-latest)",
  "rebase conflict the agent could not resolve",
  "checks failed: lint",
  "the fix still fails: e2e",
  "checks timed out after 2h",
];

const now = Date.now();
const EXITS: Exit[] = Array.from({ length: 900 }, (_, i) => {
  const login = pick(DEVS, 1.6);
  const p = persona[login];
  const repo = pick(REPOS, 1.3);
  const at = new Date(now - rand() ** 1.1 * 180 * DAY);
  const r = rand();
  const outcome: Exit["outcome"] =
    r < p.eject ? "ejected" : r < p.eject + 0.06 ? "removed" : "merged";
  const conflict = rand() < 0.25;
  const failedChecks = rand() < (outcome === "ejected" ? 0.8 : 0.2);
  const steps = [
    { step: "waiting", secs: Math.round(rand() * 1800 * p.slow) },
    { step: "held", secs: rand() < 0.15 ? Math.round(rand() * 7200) : 0 },
    { step: "rebasing", secs: conflict ? Math.round(60 + rand() * 600) : Math.round(rand() * 30) },
    { step: "checks", secs: Math.round((300 + rand() * 900) * p.slow) },
    { step: "fixing", secs: failedChecks ? Math.round(120 + rand() * 900) : 0 },
  ];
  const queued = steps.reduce((t, s) => t + s.secs, 0);
  const llm: Llm[] = [];
  const run = (kind: Llm["kind"]) => {
    const [model, effort] = pick(MODELS, 0.8);
    llm.push({
      model,
      effort,
      kind,
      cost: model.startsWith("gpt")
        ? null
        : +(0.05 + rand() * (kind === "fix" ? 1.6 : 0.6)).toFixed(2),
      failed: rand() < 0.2,
      turns: 3 + Math.floor(rand() * 30),
    });
  };
  if (conflict) run("rebase");
  if (failedChecks) run("fix");
  const number = 1000 + i;
  return {
    id: i + 1,
    repo,
    trunk: "main",
    number,
    outcome,
    detail:
      outcome === "merged"
        ? Math.floor(rand() * 2 ** 48)
            .toString(16)
            .padStart(12, "0") + "abcdef0123456789abcdef01"
        : outcome === "ejected"
          ? pick(REASONS)
          : "",
    title: pick(TITLES),
    url: `https://github.com/${repo}/pull/${number}`,
    author: { login, avatar_url: `https://github.com/${login.replace("[bot]", "")}.png?size=40` },
    fork: rand() < 0.05,
    enqueued_at: new Date(at.getTime() - queued * 1000).toISOString(),
    steps,
    at: at.toISOString(),
    llm,
  };
}).sort((a, b) => b.at.localeCompare(a.at));

const pct = (xs: number[], q: number) => {
  if (!xs.length) return null;
  const s = [...xs].sort((a, b) => a - b);
  return s[Math.min(s.length - 1, Math.floor(q * s.length))];
};
const queuedSecs = (e: Exit) => e.steps.reduce((t, s) => t + s.secs, 0);

function stats(exits: Exit[]) {
  const merged = exits.filter((e) => e.outcome === "merged");
  const times = merged.map(queuedSecs);
  const runs = (kind: Llm["kind"]) => {
    const rs = exits.flatMap((e) => e.llm.filter((l) => l.kind === kind));
    return {
      runs: rs.length,
      failed: rs.filter((l) => l.failed).length,
      turns: rs.reduce((t, l) => t + l.turns, 0),
      cost_usd: rs.reduce((t, l) => t + (l.cost ?? 0), 0),
      unpriced: rs.filter((l) => l.cost === null).length,
    };
  };
  return {
    merged: merged.length,
    ejected: exits.filter((e) => e.outcome === "ejected").length,
    removed: exits.filter((e) => e.outcome === "removed").length,
    p50_secs: pct(times, 0.5),
    p95_secs: pct(times, 0.95),
    // Proposed: what the agents spent on this slice's PRs.
    llm_cost_usd: runs("rebase").cost_usd + runs("fix").cost_usd,
    llm_runs: runs("rebase").runs + runs("fix").runs,
    runs,
  };
}

function historyStats(q: URLSearchParams) {
  const days = Number(q.get("days") ?? 30);
  const repo = q.get("repo");
  const author = q.get("author");
  const inWindow = (from: number, to: number) =>
    EXITS.filter((e) => {
      const t = new Date(e.at).getTime();
      return t > now - from * DAY && t <= now - to * DAY;
    });
  const cur = inWindow(days, 0);
  const prev = inWindow(days * 2, days);
  const scoped = (xs: Exit[]) =>
    xs.filter((e) => (!repo || e.repo === repo) && (!author || e.author?.login === author));
  const byRepo = (xs: Exit[]) => {
    const ys = xs.filter((e) => !author || e.author?.login === author);
    const { runs: _, ...all } = stats(ys);
    return [
      ...REPOS.filter((r) => ys.some((e) => e.repo === r)).map((r) => {
        const { runs: _r, ...s } = stats(ys.filter((e) => e.repo === r));
        return { repo: r, ...s };
      }),
      { repo: null, ...all },
    ];
  };
  const byDev = (xs: Exit[]) => {
    const ys = xs.filter((e) => !repo || e.repo === repo);
    return DEVS.filter((d) => ys.some((e) => e.author?.login === d)).map((d) => {
      const mine = ys.filter((e) => e.author?.login === d);
      const { runs: _, ...s } = stats(mine);
      return { login: d, avatar_url: mine[0].author?.avatar_url ?? "", ...s };
    });
  };
  const sc = scoped(cur);
  const merged_by_day = Array.from({ length: days }, (_, i) => {
    const start = now - (days - i) * DAY;
    return sc.filter(
      (e) =>
        e.outcome === "merged" &&
        new Date(e.at).getTime() > start &&
        new Date(e.at).getTime() <= start + DAY,
    ).length;
  });
  const mergedSc = sc.filter((e) => e.outcome === "merged");
  const steps = mergedSc.length
    ? STEPS.map((step) => {
        const xs = mergedSc.map((e) => e.steps.find((s) => s.step === step)?.secs ?? 0);
        return {
          step,
          mean_secs: xs.reduce((t, x) => t + x, 0) / xs.length,
          p50_secs: pct(xs, 0.5),
          p95_secs: pct(xs, 0.95),
        };
      })
    : [];
  const llms = MODELS.map(([model, effort]) => {
    const { runs } = stats(sc.map((e) => ({ ...e, llm: e.llm.filter((l) => l.model === model) })));
    return { model, effort, rebase: runs("rebase"), fix: runs("fix") };
  })
    .filter((l) => l.rebase.runs + l.fix.runs > 0)
    .sort((a, b) => b.rebase.cost_usd + b.fix.cost_usd - a.rebase.cost_usd - a.fix.cost_usd);
  return {
    current: byRepo(cur),
    previous: byRepo(prev),
    merged_by_day,
    steps,
    llms,
    developers: byDev(cur),
    developers_previous: byDev(prev),
  };
}

function history(q: URLSearchParams) {
  const days = Number(q.get("days") ?? 30);
  const before = q.get("before") ? Number(q.get("before")) : null;
  const xs = EXITS.filter(
    (e) =>
      new Date(e.at).getTime() > now - days * DAY &&
      (!q.get("repo") || e.repo === q.get("repo")) &&
      (!q.get("author") || e.author?.login === q.get("author")) &&
      (!q.get("outcome") || e.outcome === q.get("outcome")),
  );
  const start = before === null ? 0 : xs.findIndex((e) => e.id === before) + 1;
  const page = xs.slice(start, start + 30).map(({ llm: _, ...e }) => e);
  return { exits: page, next: start + 30 < xs.length ? (page.at(-1)?.id ?? null) : null };
}

const routes: [RegExp, (q: URLSearchParams) => unknown][] = [
  [
    /^\/api\/session$/,
    () => ({
      status: "signed_in",
      setup_unlocked: true,
      user: { login: "octocat", avatar_url: "https://github.com/octocat.png?size=40" },
      app: {
        slug: "mergepatrol",
        html_url: "https://github.com/apps/mergepatrol",
        install_url: "#",
      },
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
    }),
  ],
  [
    /^\/api\/w\/1\/settings$/,
    () => ({
      api_keys: [{ provider: "anthropic", key: { hint: "sk-…abcd" } }],
      chatgpt: { login: null, sign_in: null },
    }),
  ],
  [/^\/api\/w\/1\/billing$/, () => null],
  [/^\/api\/w\/1\/history\/stats$/, historyStats],
  [/^\/api\/w\/1\/history$/, history],
];

export function prototypeStatsMock(): Plugin {
  return {
    name: "prototype-stats-mock",
    configureServer(server) {
      server.middlewares.use((req, res, next) => {
        const url = new URL((req as { url?: string }).url ?? "/", "http://x");
        if (!url.pathname.startsWith("/api/")) return next();
        const route = routes.find(([re]) => re.test(url.pathname));
        res.setHeader("content-type", "application/json");
        if (!route) {
          res.statusCode = 404;
          return res.end(JSON.stringify({}));
        }
        res.end(JSON.stringify(route[1](url.searchParams)));
      });
    },
  };
}
