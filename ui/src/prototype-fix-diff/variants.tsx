/*
 * PROTOTYPE, throw away. Four ways the Activity feed could show the fix agent's diff, rendered
 * with @pierre/diffs (diffs.com), switchable with `?variant=A..D` and the bar at the bottom.
 * The fake API in mock.ts serves the PR, its events, and each fix's patch.
 */
import { parsePatchFiles } from "@pierre/diffs";
import { FileDiff } from "@pierre/diffs/react";
import {
  type CSSProperties,
  type ReactNode,
  useEffect,
  useMemo,
  useState,
  useSyncExternalStore,
} from "react";
import type { EventView } from "../api/EventView";
import { timeAgo } from "../ui";
import { fixPatches, inProgressPatch, phase, setPhase } from "./mock";

// --- Switcher ---------------------------------------------------------------------------------

export const VARIANTS = {
  A: "Expandable rows",
  B: "Attempt cards",
  C: "Diff drawer",
  D: "Live while fixing",
} as const;
type Key = keyof typeof VARIANTS;
const KEYS = Object.keys(VARIANTS) as Key[];

const read = (): Key => {
  const v =
    new URLSearchParams(location.search).get("variant") ??
    localStorage.getItem("proto-fix-variant");
  return KEYS.includes(v as Key) ? (v as Key) : "A";
};
const listeners = new Set<() => void>();
const write = (v: Key) => {
  localStorage.setItem("proto-fix-variant", v);
  const url = new URL(location.href);
  url.searchParams.set("variant", v);
  history.replaceState(history.state, "", url);
  for (const l of listeners) l();
};
const useVariant = () =>
  useSyncExternalStore((l) => {
    listeners.add(l);
    return () => listeners.delete(l);
  }, read);

/** On in dev once `?variant=` was in the URL. */
export const prototypeOn = () =>
  import.meta.env.DEV &&
  (new URLSearchParams(location.search).has("variant") ||
    !!localStorage.getItem("proto-fix-variant"));

function Switcher({ current }: { current: Key }) {
  const step = (d: number) => write(KEYS[(KEYS.indexOf(current) + d + KEYS.length) % KEYS.length]);
  useEffect(() => {
    const on = (e: KeyboardEvent) => {
      const t = e.target as HTMLElement;
      if (t.closest("input, textarea, [contenteditable]")) return;
      if (e.key === "ArrowLeft") step(-1);
      if (e.key === "ArrowRight") step(1);
    };
    addEventListener("keydown", on);
    return () => removeEventListener("keydown", on);
  });
  const p = phase();
  const btn = "rounded-full px-2.5 py-1 hover:bg-white/15";
  return (
    <div className="fixed bottom-4 left-1/2 z-50 flex -translate-x-1/2 items-center gap-1 rounded-full bg-black px-2 py-1.5 text-xs text-white shadow-2xl ring-1 ring-white/20">
      <button type="button" className={btn} onClick={() => step(-1)}>
        ←
      </button>
      <span className="min-w-44 text-center font-medium">
        {current} ({VARIANTS[current]})
      </span>
      <button type="button" className={btn} onClick={() => step(1)}>
        →
      </button>
      <span className="mx-1 h-4 w-px bg-white/30" />
      {(["fixing", "fixed"] as const).map((x) => (
        <button
          key={x}
          type="button"
          onClick={() => setPhase(x)}
          className={`${btn} ${p === x ? "bg-white text-black hover:bg-white" : ""}`}
        >
          {x === "fixing" ? "Attempt 2 running" : "Attempt 2 pushed"}
        </button>
      ))}
    </div>
  );
}

// --- Shared -----------------------------------------------------------------------------------

type FileStat = { name: string; adds: number; dels: number };
type Fix = { files: ReturnType<typeof parsePatchFiles>[number]["files"]; stats: FileStat[] };

function parseFix(patch: string): Fix {
  const files = parsePatchFiles(patch).flatMap((p) => p.files);
  const stats = patch
    .split(/^(?=diff --git )/m)
    .filter((c) => c.trim())
    .map((c) => {
      const body = c.slice(c.indexOf("@@")).split("\n");
      return {
        name: c.match(/^\+\+\+ b\/(.+)$/m)?.[1] ?? "?",
        adds: body.filter((l) => l.startsWith("+")).length,
        dels: body.filter((l) => l.startsWith("-")).length,
      };
    });
  return { files, stats };
}

const useFix = (patch: string | undefined) =>
  useMemo(() => (patch ? parseFix(patch) : null), [patch]);

const total = (s: FileStat[]) => ({
  adds: s.reduce((t, f) => t + f.adds, 0),
  dels: s.reduce((t, f) => t + f.dels, 0),
});

function Plus({ stats }: { stats: FileStat[] | FileStat }) {
  const { adds, dels } = Array.isArray(stats) ? total(stats) : stats;
  return (
    <span className="font-mono tabular-nums">
      <span className="text-emerald-600 dark:text-emerald-400">+{adds}</span>{" "}
      <span className="text-red-600 dark:text-red-400">−{dels}</span>
    </span>
  );
}

const useDark = () =>
  useSyncExternalStore(
    (l) => {
      const o = new MutationObserver(l);
      o.observe(document.documentElement, { attributes: true, attributeFilter: ["class"] });
      return () => o.disconnect();
    },
    () => document.documentElement.classList.contains("dark"),
  );

function Diffs({
  fix,
  style = "unified",
  only,
}: {
  fix: Fix;
  style?: "unified" | "split";
  only?: string;
}) {
  const dark = useDark();
  return (
    <div className="space-y-2">
      {fix.files
        .filter((f) => !only || f.name === only)
        .map((f) => (
          <FileDiff
            key={f.name}
            fileDiff={f}
            className="block overflow-hidden rounded-md border border-line"
            style={{ "--diffs-font-size": "12px" } as CSSProperties}
            options={{
              diffStyle: style,
              themeType: dark ? "dark" : "light",
              overflow: "wrap",
              hunkSeparators: "line-info",
              lineDiffType: "word",
            }}
          />
        ))}
    </div>
  );
}

const isSha = (s: string) => /^[0-9a-f]{40}$/.test(s);

/** A fix attempt, from its queue `fixing` to the head it published, if it got there. */
type Attempt = {
  n: number;
  of: number;
  checks: string;
  at: string;
  spend?: string;
  verifying?: string;
  sha?: string;
  /** A later attempt began: its checks failed again. */
  superseded: boolean;
};

/** The attempts in `events`, oldest first. `events` are newest first. */
function attemptsOf(events: EventView[]): Attempt[] {
  const out: Attempt[] = [];
  for (const e of [...events].reverse()) {
    const m = e.kind === "fixing" && e.message.match(/^(.*), attempt (\d+) of (\d+)$/);
    if (m) {
      const last = out.at(-1);
      if (last) last.superseded = true;
      out.push({ n: Number(m[2]), of: Number(m[3]), checks: m[1], at: e.at, superseded: false });
      continue;
    }
    const a = out.at(-1);
    if (!a || a.sha) continue;
    if (e.kind === "verify") a.verifying = e.message;
    if (e.kind === "fix") a.spend = e.message;
    if (e.kind === "published" && a.spend) a.sha = e.message;
  }
  return out;
}

/** The attempt each event belongs to, if any. */
function attemptOfEvent(events: EventView[]) {
  const map = new Map<EventView, number>();
  let n = 0;
  let open = false;
  for (const e of [...events].reverse()) {
    const m = e.kind === "fixing" && e.message.match(/attempt (\d+) of/);
    if (m) {
      n = Number(m[1]);
      open = true;
    }
    if (open && ["fixing", "verify", "fix", "published", "job"].includes(e.kind)) map.set(e, n);
    if (open && e.kind === "published") open = false;
  }
  return map;
}

function Chips({ checks }: { checks: string }) {
  return (
    <span className="flex flex-wrap gap-1">
      {checks.split(", ").map((c) => (
        <span
          key={c}
          className="rounded bg-red-50 px-1.5 py-px font-mono text-[11px] text-red-700 dark:bg-red-400/10 dark:text-red-300"
        >
          ✕ {c}
        </span>
      ))}
    </span>
  );
}

function Spend({ spend }: { spend: string }) {
  return (
    <span className="rounded bg-violet-50 px-1.5 py-px text-violet-700 tabular-nums dark:bg-violet-400/10 dark:text-violet-300">
      ✦ {spend.replace(", ", " · ")}
    </span>
  );
}

function ShaLink({ repo, sha }: { repo: string; sha: string }) {
  return (
    <a
      href={`https://github.com/${repo}/commit/${sha}`}
      target="_blank"
      rel="noreferrer"
      className="rounded bg-sunken px-1 py-px font-mono text-xs text-fg-2 hover:text-fg hover:underline"
    >
      {sha.slice(0, 7)}
    </a>
  );
}

function Working({ what }: { what: string }) {
  return (
    <span className="inline-flex items-center gap-1.5 text-amber-700 dark:text-amber-300">
      <span className="size-1.5 animate-pulse rounded-full bg-current" />
      {what}
    </span>
  );
}

type FeedProps = {
  events: EventView[];
  shown: EventView[];
  repo: string;
  row: (e: EventView, extra?: ReactNode) => ReactNode;
};

// --- A: Expandable rows -----------------------------------------------------------------------
// The feed as it is. A fix's `published` row says what changed and opens the diff in place.

function VariantA({ events, shown, repo, row }: FeedProps) {
  const [open, setOpen] = useState<string | null>(null);
  const running = attemptsOf(events).find((a) => !a.sha);
  return (
    <table className="w-full text-xs">
      <tbody>
        {shown.map((e, i) => {
          const patch = e.kind === "published" ? fixPatches[e.message] : undefined;
          if (e.kind === "fixing" && running && e.at === running.at)
            return row(
              e,
              <span className="flex flex-wrap items-center gap-2">
                <Chips checks={running.checks} />
                <Working what="Claude is fixing" />
              </span>,
            );
          if (!patch) return row(e);
          return (
            <ExpandableRow
              key={e.at}
              e={e}
              i={i}
              patch={patch}
              repo={repo}
              open={open === e.message}
              toggle={() => setOpen(open === e.message ? null : e.message)}
            />
          );
        })}
      </tbody>
    </table>
  );
}

function ExpandableRow({
  e,
  patch,
  repo,
  open,
  toggle,
}: {
  e: EventView;
  i: number;
  patch: string;
  repo: string;
  open: boolean;
  toggle: () => void;
}) {
  const fix = useFix(patch);
  if (!fix) return null;
  return (
    <>
      <tr className="border-b border-line-soft align-baseline">
        <td className="w-16 py-1.5 pr-3 whitespace-nowrap text-faint tabular-nums">
          {timeAgo(e.at)}
        </td>
        <td className="w-20 py-1.5 pr-3 font-medium">Fix pushed</td>
        <td className="py-1.5 text-muted">
          <span className="flex flex-wrap items-center gap-2">
            <ShaLink repo={repo} sha={e.message} />
            <button
              type="button"
              onClick={toggle}
              className="rounded border border-line px-1.5 py-px text-fg-2 hover:bg-sunken"
            >
              {open ? "▾" : "▸"} {fix.stats.length} files · <Plus stats={fix.stats} />
            </button>
          </span>
        </td>
      </tr>
      {open && (
        <tr>
          <td colSpan={3} className="border-b border-line-soft py-2">
            <Diffs fix={fix} />
          </td>
        </tr>
      )}
    </>
  );
}

// --- B: Attempt cards -------------------------------------------------------------------------
// Each attempt's events fold into one card: what failed, what the agent spent, what it changed.
// One file at a time, picked from its tabs. The newest card is open.

function VariantB({ events, shown, repo, row, live }: FeedProps & { live?: boolean }) {
  const attempts = attemptsOf(events);
  const owner = attemptOfEvent(events);
  const done = new Set<number>();
  return (
    <div className="space-y-2">
      <table className="w-full text-xs">
        <tbody>
          {shown.map((e) => {
            const n = owner.get(e);
            if (n === undefined) return row(e);
            if (done.has(n)) return null;
            done.add(n);
            const a = attempts.find((x) => x.n === n);
            if (!a) return null;
            return (
              <tr key={`card ${n}`}>
                <td colSpan={3} className="py-2">
                  <AttemptCard
                    a={a}
                    repo={repo}
                    initiallyOpen={a === attempts.at(-1)}
                    live={live}
                  />
                </td>
              </tr>
            );
          })}
        </tbody>
      </table>
    </div>
  );
}

function AttemptCard({
  a,
  repo,
  initiallyOpen,
  live,
}: {
  a: Attempt;
  repo: string;
  initiallyOpen: boolean;
  live?: boolean;
}) {
  const [open, setOpen] = useState(initiallyOpen);
  const fix = useFix(a.sha ? fixPatches[a.sha] : undefined);
  const [file, setFile] = useState<string | null>(null);
  const status = !a.sha ? (
    <Working what={a.verifying ? `Verifying \`${a.verifying}\`` : "Claude is fixing"} />
  ) : a.superseded ? (
    <span className="text-red-700 dark:text-red-300">Checks failed again</span>
  ) : (
    <span className="text-amber-700 dark:text-amber-300">Pushed · waiting for checks</span>
  );
  return (
    <div className="overflow-hidden rounded-lg border border-line bg-surface">
      <button
        type="button"
        onClick={() => setOpen(!open)}
        className="flex w-full flex-wrap items-center gap-x-3 gap-y-1 px-3 py-2 text-left text-xs hover:bg-sunken/50"
      >
        <span className="text-faint">{open ? "▾" : "▸"}</span>
        <span className="font-semibold">
          Fix {a.n} of {a.of}
        </span>
        <Chips checks={a.checks} />
        {a.spend && <Spend spend={a.spend} />}
        {fix && (
          <span className="text-muted">
            {fix.stats.length} files · <Plus stats={fix.stats} />
          </span>
        )}
        <span className="ml-auto flex items-center gap-2">
          {status}
          {a.sha && <ShaLink repo={repo} sha={a.sha} />}
          <span className="text-faint tabular-nums">{timeAgo(a.at)}</span>
        </span>
      </button>
      {open && !a.sha && live && <LiveDiff verifying={a.verifying} />}
      {open && fix && (
        <div className="border-t border-line-soft p-2">
          <div className="mb-2 flex flex-wrap gap-1">
            {fix.stats.map((s) => {
              const on = (file ?? fix.stats[0].name) === s.name;
              return (
                <button
                  key={s.name}
                  type="button"
                  onClick={() => setFile(s.name)}
                  title={s.name}
                  className={`rounded-md border px-2 py-0.5 font-mono text-[11px] ${on ? "border-fg bg-fg text-surface" : "border-line text-fg-2 hover:bg-sunken"}`}
                >
                  {s.name.split("/").at(-1)} <Plus stats={s} />
                </button>
              );
            })}
          </div>
          <Diffs fix={fix} only={file ?? fix.stats[0].name} />
        </div>
      )}
    </div>
  );
}

// --- C: Diff drawer ---------------------------------------------------------------------------
// The feed stays one line per event. A fix opens a drawer over the page: every attempt in tabs,
// a file list, and a split diff with room to read it.

function VariantC({ events, shown, repo, row }: FeedProps) {
  const attempts = attemptsOf(events).filter((a) => a.sha);
  const [at, setAt] = useState<number | null>(null);
  const running = attemptsOf(events).find((a) => !a.sha);
  useEffect(() => {
    const on = (e: KeyboardEvent) => e.key === "Escape" && setAt(null);
    addEventListener("keydown", on);
    return () => removeEventListener("keydown", on);
  }, []);
  return (
    <>
      <table className="w-full text-xs">
        <tbody>
          {shown.map((e) => {
            if (e.kind === "fixing" && running && e.at === running.at)
              return row(
                e,
                <span className="flex flex-wrap items-center gap-2">
                  <Chips checks={running.checks} />
                  <Working what="Claude is fixing" />
                </span>,
              );
            const n = e.kind === "published" ? attempts.findIndex((a) => a.sha === e.message) : -1;
            if (n === -1) return row(e);
            return row(
              e,
              <span className="flex items-center gap-2">
                <ShaLink repo={repo} sha={e.message} />
                <button
                  type="button"
                  onClick={() => setAt(n)}
                  className="text-fg-2 underline underline-offset-2 hover:text-fg"
                >
                  View fix {attempts[n].n}
                </button>
              </span>,
            );
          })}
        </tbody>
      </table>
      {at !== null && <Drawer attempts={attempts} at={at} setAt={setAt} repo={repo} />}
    </>
  );
}

function Drawer({
  attempts,
  at,
  setAt,
  repo,
}: {
  attempts: Attempt[];
  at: number;
  setAt: (n: number | null) => void;
  repo: string;
}) {
  const a = attempts[at];
  const fix = useFix(a.sha ? fixPatches[a.sha] : undefined);
  const [file, setFile] = useState<string | null>(null);
  if (!fix) return null;
  return (
    <div className="fixed inset-0 z-40 flex justify-end bg-black/30" onClick={() => setAt(null)}>
      <div
        className="flex h-full w-[min(1100px,75vw)] flex-col bg-page shadow-2xl"
        onClick={(e) => e.stopPropagation()}
      >
        <div className="flex items-center gap-3 border-b border-line bg-surface px-4 py-3 text-xs">
          <div className="flex gap-1">
            {attempts.map((x, i) => (
              <button
                key={x.n}
                type="button"
                onClick={() => {
                  setAt(i);
                  setFile(null);
                }}
                className={`rounded px-2 py-1 font-medium ${i === at ? "bg-fg text-surface" : "text-muted hover:bg-sunken"}`}
              >
                Fix {x.n}
              </button>
            ))}
          </div>
          <Chips checks={a.checks} />
          {a.spend && <Spend spend={a.spend} />}
          {a.sha && <ShaLink repo={repo} sha={a.sha} />}
          <button
            type="button"
            onClick={() => setAt(null)}
            className="ml-auto text-muted hover:text-fg"
          >
            Esc ✕
          </button>
        </div>
        <div className="flex min-h-0 flex-1">
          <nav className="w-60 shrink-0 overflow-y-auto border-r border-line p-2 text-xs">
            <button
              type="button"
              onClick={() => setFile(null)}
              className={`mb-1 w-full rounded px-2 py-1 text-left ${file === null ? "bg-sunken font-medium" : "hover:bg-sunken"}`}
            >
              All {fix.stats.length} files <Plus stats={fix.stats} />
            </button>
            {fix.stats.map((s) => (
              <button
                key={s.name}
                type="button"
                onClick={() => setFile(s.name)}
                title={s.name}
                className={`flex w-full items-center justify-between gap-2 rounded px-2 py-1 text-left font-mono text-[11px] ${file === s.name ? "bg-sunken font-medium" : "hover:bg-sunken"}`}
              >
                <span className="truncate">{s.name}</span>
                <Plus stats={s} />
              </button>
            ))}
          </nav>
          <div className="min-w-0 flex-1 overflow-y-auto p-3">
            <Diffs fix={fix} style="split" only={file ?? undefined} />
          </div>
        </div>
      </div>
    </div>
  );
}

// --- D: Live while fixing ---------------------------------------------------------------------
// The attempt cards of B, but the running attempt shows the agent's working tree as it edits.
// Needs the sandbox to stream `git diff` out of the job: the queue sees no diff before the commit.

/** `patch` with only its first `n` hunk lines, the hunk headers counted to match. */
function partial(patch: string, n: number) {
  const out: string[] = [];
  let left = n;
  for (const chunk of patch.split(/^(?=diff --git )/m).filter((c) => c.trim())) {
    if (left <= 0) break;
    const lines = chunk.trimEnd().split("\n");
    const at = lines.findIndex((l) => l.startsWith("@@"));
    out.push(...lines.slice(0, at));
    let i = at;
    while (i < lines.length && left > 0) {
      const m = lines[i].match(/^@@ -(\d+),\d+ \+(\d+),\d+ @@(.*)$/);
      let j = i + 1;
      const body: string[] = [];
      while (j < lines.length && !lines[j].startsWith("@@") && left > 0) {
        body.push(lines[j]);
        j++;
        left--;
      }
      const o = body.filter((l) => l[0] !== "+").length;
      const nw = body.filter((l) => l[0] !== "-").length;
      if (m) out.push(`@@ -${m[1]},${o} +${m[2]},${nw} @@${m[3]}`, ...body);
      while (j < lines.length && !lines[j].startsWith("@@")) j++;
      i = j;
    }
  }
  return `${out.join("\n")}\n`;
}

function LiveDiff({ verifying }: { verifying?: string }) {
  const lines = inProgressPatch
    .split("\n")
    .filter((l) => /^[ +-]/.test(l) && !/^(\+\+\+|---) /.test(l)).length;
  const [n, setN] = useState(1);
  useEffect(() => {
    const t = setInterval(() => setN((x) => (x >= lines + 12 ? 1 : x + 1)), 280);
    return () => clearInterval(t);
  }, [lines]);
  const fix = useMemo(() => parseFix(partial(inProgressPatch, Math.min(n, lines))), [n, lines]);
  const editing = fix.stats.at(-1)?.name;
  const doneEditing = n > lines;
  return (
    <div className="border-t border-line-soft p-2">
      <div className="mb-2 flex items-center gap-3 text-xs text-muted">
        {doneEditing ? (
          <Working what={`Verifying \`${verifying ?? "cargo test"}\``} />
        ) : (
          <Working what={`Editing ${editing ?? "…"} · turn ${1 + Math.floor(n / 4)}`} />
        )}
        <span className="ml-auto">
          Working tree · <Plus stats={fix.stats} />
        </span>
      </div>
      <Diffs fix={fix} />
    </div>
  );
}

// --- Mount ------------------------------------------------------------------------------------

export function PrototypeFeed(props: FeedProps) {
  const v = useVariant();
  return (
    <>
      {v === "A" && <VariantA {...props} />}
      {v === "B" && <VariantB {...props} />}
      {v === "C" && <VariantC {...props} />}
      {v === "D" && <VariantB {...props} live />}
      <Switcher current={v} />
    </>
  );
}
