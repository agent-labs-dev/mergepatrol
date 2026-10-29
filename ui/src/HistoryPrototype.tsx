// PROTOTYPE, throwaway: developer-level stats, and History broken out of one pane, three ways.
// Switch with `?variant=` (0 = today's page, A, B, C) or the bar at the bottom. Run on fake data
// with `pnpm prototype:stats`. The data adds two things to the API: an `author` filter on
// /history and /history/stats, and a `developers` list beside `current`.
import { useInfiniteQuery, useQuery } from "@tanstack/react-query";
import { useNavigate, useSearch } from "@tanstack/react-router";
import { useState } from "react";
import type { ExitsPage } from "./api/ExitsPage";
import type { HistoryStatsView } from "./api/HistoryStatsView";
import type { OutcomeView } from "./api/OutcomeView";
import type { StatsView } from "./api/StatsView";
import {
  Bar,
  History,
  Llms,
  OUTCOMES,
  Row,
  Segmented,
  Stats,
  Steps,
  stepInfo,
  WINDOWS,
} from "./History";
import { PrototypeSwitcher } from "./PrototypeSwitcher";
import { Avatar, duration, secondary, usd } from "./ui";

const VARIANTS: [string, string][] = [
  ["0", "Today's page"],
  ["A", "Sub-tabs + scope chips"],
  ["B", "Pivot rail"],
  ["C", "Leaderboard + drawer"],
];

/** Proposed: StatsView gains what the agents spent. */
type Slice = Omit<StatsView, "repo"> & { llm_cost_usd: number; llm_runs: number };
/** Proposed: one developer's slice of the window. */
type DevStats = Slice & { login: string; avatar_url: string };
type Proposed = Omit<HistoryStatsView, "current" | "previous"> & {
  current: (StatsView & Slice)[];
  previous: (StatsView & Slice)[];
  developers: DevStats[];
  developers_previous: DevStats[];
};

type Q = { repo?: string; author?: string; days: number; outcome?: OutcomeView };
const qs = (q: Record<string, string | number | undefined>) =>
  new URLSearchParams(
    Object.entries(q).flatMap(([k, v]) => (v === undefined ? [] : [[k, String(v)]])),
  ).toString();

function useStats(workspace: number, q: Omit<Q, "outcome">) {
  return useQuery({
    queryKey: ["proto-stats", workspace, q],
    queryFn: async () =>
      (await fetch(`/api/w/${workspace}/history/stats?${qs(q)}`)).json() as Promise<Proposed>,
  });
}

function useExits(workspace: number, q: Q) {
  return useInfiniteQuery({
    queryKey: ["proto-exits", workspace, q],
    queryFn: async ({ pageParam }) =>
      (
        await fetch(`/api/w/${workspace}/history?${qs({ ...q, before: pageParam ?? undefined })}`)
      ).json() as Promise<ExitsPage>,
    initialPageParam: null as number | null,
    getNextPageParam: (p) => p.next,
  });
}

type Search = {
  variant?: string;
  repo?: string;
  author?: string;
  days?: 7 | 30 | 90;
  outcome?: OutcomeView;
  view?: string;
  by?: string;
  sort?: string;
};

function useProtoSearch() {
  const search = useSearch({ strict: false }) as Search;
  const navigate = useNavigate();
  const set = (patch: Partial<Search>) =>
    // biome-ignore lint/suspicious/noExplicitAny: prototype
    navigate({ to: ".", search: (s: any) => ({ ...s, ...patch }) } as any);
  return { ...search, days: search.days ?? 30, set };
}

export function HistoryPrototype({ workspace }: { workspace: number }) {
  const { variant = "A" } = useProtoSearch();
  return (
    <>
      {variant === "0" && <History workspace={workspace} />}
      {variant === "A" && <VariantA workspace={workspace} />}
      {variant === "B" && <VariantB workspace={workspace} />}
      {variant === "C" && <VariantC workspace={workspace} />}
      <PrototypeSwitcher variants={VARIANTS} current={variant} />
    </>
  );
}

// ---------------------------------------------------------------- shared bits (not layout)

const isBot = (login: string) => login.endsWith("[bot]");
const ejectRate = (s?: Slice) => {
  const tried = (s?.merged ?? 0) + (s?.ejected ?? 0);
  return tried ? (s?.ejected ?? 0) / tried : null;
};
const pctText = (r: number | null) => (r === null ? "–" : `${Math.round(r * 100)}%`);
const perMerge = (s: Slice) => (s.merged ? usd(s.llm_cost_usd / s.merged) : "–");

function ExitList({ workspace, q, showRepo }: { workspace: number; q: Q; showRepo: boolean }) {
  const exits = useExits(workspace, q);
  const rows = exits.data?.pages.flatMap((p) => p.exits) ?? [];
  return (
    <>
      {exits.data && rows.length === 0 && (
        <p className="p-6 text-faint">No PR left the queue in the last {q.days} days.</p>
      )}
      <ol>
        {rows.map((e) => (
          <li key={e.id}>
            <Row exit={e} showRepo={showRepo} />
          </li>
        ))}
      </ol>
      {exits.hasNextPage && (
        <div className="p-4 text-center">
          <button type="button" className={secondary} onClick={() => exits.fetchNextPage()}>
            {exits.isFetchingNextPage ? "Loading…" : "Load more"}
          </button>
        </div>
      )}
    </>
  );
}

function WindowPicker() {
  const { days, set } = useProtoSearch();
  return (
    <Segmented
      name="window"
      label="Window"
      options={WINDOWS}
      value={days as 7 | 30 | 90}
      text={(d) => `${d}d`}
      onChange={(days) => set({ days })}
    />
  );
}

function OutcomePicker() {
  const { outcome, set } = useProtoSearch();
  return (
    <Segmented
      name="outcome"
      label="Outcome"
      options={[undefined, ...OUTCOMES]}
      value={outcome}
      text={(o) => (o ? o[0].toUpperCase() + o.slice(1) : "All")}
      onChange={(outcome) => set({ outcome })}
    />
  );
}

function Dev({ login, avatar, size = "size-5" }: { login: string; avatar: string; size?: string }) {
  return (
    <span className="flex min-w-0 items-center gap-2">
      <Avatar src={avatar} alt={login} size={size} />
      <span className="truncate">{login.replace("[bot]", "")}</span>
      {isBot(login) && (
        <span className="rounded bg-sunken px-1 text-[10px] text-muted uppercase">bot</span>
      )}
    </span>
  );
}

// ---------------------------------------------------------------- A: sub-tabs + scope chips

/** History becomes a section with its own tabs: Overview, Repos, Developers, PRs. A repo and a
 * developer are scope chips that follow you across them, so "octocat in acme/api" is two
 * clicks and every tab answers for it. */
function VariantA({ workspace }: { workspace: number }) {
  const { view = "overview", repo, author, days, outcome, set } = useProtoSearch();
  const stats = useStats(workspace, { repo, author, days });
  if (!stats.data) return null;
  const d = stats.data;
  const tabs = [
    ["overview", "Overview"],
    ["repos", "Repos"],
    ["developers", "Developers"],
    ["prs", "Pull requests"],
  ] as const;
  const scopeLabel = [author, repo].filter(Boolean).join(" in ") || "Everything";
  return (
    <div className="h-full overflow-y-auto bg-surface">
      <div className="sticky top-0 z-10 border-b border-line bg-surface px-4 sm:px-6">
        <div className="flex flex-wrap items-center justify-between gap-3 pt-3">
          <div className="flex flex-wrap items-center gap-2 text-sm">
            <span className="text-muted">Scope</span>
            <Chip label="Repo" value={repo} onClear={() => set({ repo: undefined })} />
            <Chip label="Developer" value={author} onClear={() => set({ author: undefined })} />
            {!repo && !author && <span className="text-faint">every repo, every developer</span>}
          </div>
          <WindowPicker />
        </div>
        <nav className="mt-2 flex gap-4 text-sm">
          {tabs.map(([k, label]) => (
            <button
              key={k}
              type="button"
              onClick={() => set({ view: k })}
              className={`-mb-px border-b-2 pb-2 ${view === k ? "border-fg text-fg" : "border-transparent text-muted hover:text-fg"}`}
            >
              {label}
            </button>
          ))}
        </nav>
      </div>
      <div className="px-4 py-4 sm:px-6">
        {view === "overview" && (
          <>
            <p className="font-medium">{scopeLabel}</p>
            <Stats
              current={repo ? d.current.find((s) => s.repo === repo) : d.current.at(-1)}
              previous={repo ? d.previous.find((s) => s.repo === repo) : d.previous.at(-1)}
              mergedByDay={d.merged_by_day}
              days={days}
            />
            <Steps steps={d.steps} />
            <Llms llms={d.llms} days={days} />
          </>
        )}
        {view === "repos" && (
          <SliceTable
            rows={d.current
              .filter((s) => s.repo)
              .map((s) => ({
                key: s.repo ?? "",
                name: <span className="font-mono">{s.repo}</span>,
                s,
                before: d.previous.find((p) => p.repo === s.repo),
              }))}
            selected={repo}
            onPick={(r) => set({ repo: r, view: "overview" })}
            what="repo"
          />
        )}
        {view === "developers" && (
          <SliceTable
            rows={d.developers.map((s) => ({
              key: s.login,
              name: <Dev login={s.login} avatar={s.avatar_url} />,
              s,
              before: d.developers_previous.find((p) => p.login === s.login),
            }))}
            selected={author}
            onPick={(a) => set({ author: a, view: "overview" })}
            what="developer"
          />
        )}
      </div>
      {view === "prs" && (
        <>
          <div className="flex justify-end px-4 pb-2 sm:px-6">
            <OutcomePicker />
          </div>
          <div className="border-t border-line">
            <ExitList workspace={workspace} q={{ repo, author, days, outcome }} showRepo={!repo} />
          </div>
        </>
      )}
    </div>
  );
}

function Chip({ label, value, onClear }: { label: string; value?: string; onClear: () => void }) {
  if (!value) return null;
  return (
    <span className="flex items-center gap-1 rounded-full bg-sunken py-0.5 pr-1 pl-2.5 ring-1 ring-line">
      <span className="text-muted">{label}:</span> {value}
      <button
        type="button"
        onClick={onClear}
        className="rounded-full px-1.5 text-muted hover:bg-surface hover:text-fg"
        aria-label={`Clear ${label}`}
      >
        ×
      </button>
    </span>
  );
}

function SliceTable({
  rows,
  selected,
  onPick,
  what,
}: {
  rows: { key: string; name: React.ReactNode; s: Slice; before?: Slice }[];
  selected?: string;
  onPick: (key: string) => void;
  what: string;
}) {
  return (
    <table className="w-full text-sm">
      <thead className="text-xs text-faint">
        <tr>
          <th className="py-2 text-left font-normal capitalize">{what}</th>
          <th className="py-2 text-right font-normal">Merged</th>
          <th className="py-2 text-right font-normal">Eject rate</th>
          <th className="py-2 text-right font-normal">Median</th>
          <th className="py-2 text-right font-normal max-sm:hidden">p95</th>
          <th className="py-2 text-right font-normal max-sm:hidden">LLM spend</th>
          <th className="py-2 text-right font-normal max-sm:hidden">Per merge</th>
        </tr>
      </thead>
      <tbody className="tabular-nums">
        {rows.map(({ key, name, s, before }) => {
          const change = s.merged - (before?.merged ?? 0);
          return (
            <tr
              key={key}
              onClick={() => onPick(key)}
              className={`cursor-pointer border-t border-line-soft hover:bg-sunken ${key === selected ? "bg-sunken" : ""}`}
            >
              <td className="py-2">{name}</td>
              <td className="py-2 text-right">
                {s.merged}{" "}
                {change !== 0 && (
                  <span className={`text-xs ${change > 0 ? "text-ok" : "text-bad"}`}>
                    {change > 0 ? "▲" : "▼"}
                    {Math.abs(change)}
                  </span>
                )}
              </td>
              <td className="py-2 text-right">{pctText(ejectRate(s))}</td>
              <td className="py-2 text-right">
                {s.p50_secs === null ? "–" : duration(s.p50_secs)}
              </td>
              <td className="py-2 text-right text-muted max-sm:hidden">
                {s.p95_secs === null ? "–" : duration(s.p95_secs)}
              </td>
              <td className="py-2 text-right max-sm:hidden">{usd(s.llm_cost_usd)}</td>
              <td className="py-2 text-right text-muted max-sm:hidden">{perMerge(s)}</td>
            </tr>
          );
        })}
      </tbody>
    </table>
  );
}

// ---------------------------------------------------------------- B: pivot rail

/** Today's two panes, with the rail pivoting between repos and developers. Pick one and the
 * right pane is theirs: the same metrics, the other dimension broken down beneath, then their
 * PRs. Picking from the breakdown narrows to both. */
function VariantB({ workspace }: { workspace: number }) {
  const { by = "repos", repo, author, days, outcome, set } = useProtoSearch();
  // The rail always lists the whole window: the other filter only narrows the right pane.
  const all = useStats(workspace, { days });
  const scoped = useStats(workspace, { repo, author, days });
  if (!all.data || !scoped.data) return null;
  const a = all.data;
  const d = scoped.data;
  const cur = repo ? d.current.find((s) => s.repo === repo) : d.current.at(-1);
  const prev = repo ? d.previous.find((s) => s.repo === repo) : d.previous.at(-1);
  const title =
    [author, repo].filter(Boolean).join(" in ") ||
    (by === "repos" ? "All repos" : "All developers");
  return (
    <div className="flex h-full flex-col md:grid md:grid-cols-[240px_1fr]">
      <div className="shrink-0 border-line p-2 max-md:border-b md:overflow-y-auto md:border-r">
        <div className="px-1 py-1">
          <Segmented
            name="by"
            label="List"
            options={["repos", "developers"]}
            value={by}
            text={(o) => (o === "repos" ? "Repos" : "Developers")}
            onChange={(by) => set({ by, repo: undefined, author: undefined })}
          />
        </div>
        <RailButton
          label={by === "repos" ? "All repos" : "All developers"}
          count={a.current.at(-1)?.merged ?? 0}
          selected={by === "repos" ? !repo : !author}
          onClick={() => set(by === "repos" ? { repo: undefined } : { author: undefined })}
        />
        {by === "repos"
          ? a.current
              .filter((s) => s.repo)
              .map((s) => (
                <RailButton
                  key={s.repo}
                  label={s.repo?.split("/")[1] ?? ""}
                  count={s.merged}
                  selected={s.repo === repo}
                  onClick={() => set({ repo: s.repo ?? undefined })}
                />
              ))
          : a.developers
              .slice()
              .sort((x, y) => y.merged - x.merged)
              .map((s) => (
                <RailButton
                  key={s.login}
                  label={<Dev login={s.login} avatar={s.avatar_url} size="size-4" />}
                  count={s.merged}
                  selected={s.login === author}
                  onClick={() => set({ author: s.login })}
                />
              ))}
      </div>
      <div className="min-h-0 flex-1 overflow-y-auto bg-surface">
        <div className="border-b border-line px-4 py-4 sm:px-6">
          <div className="flex flex-wrap items-center justify-between gap-3">
            <p className="flex items-center gap-2 font-medium">
              {title}
              {(by === "repos" ? author : repo) && (
                <button
                  type="button"
                  className="text-xs font-normal text-muted hover:text-fg"
                  onClick={() => set(by === "repos" ? { author: undefined } : { repo: undefined })}
                >
                  × {by === "repos" ? author : repo}
                </button>
              )}
            </p>
            <div className="flex flex-wrap gap-3">
              <OutcomePicker />
              <WindowPicker />
            </div>
          </div>
          <Stats current={cur} previous={prev} mergedByDay={d.merged_by_day} days={days} />
          <Steps steps={d.steps} />
          {by === "repos" ? (
            <Breakdown
              title={repo ? "Developers in this repo" : "Developers"}
              rows={d.developers.map((s) => ({
                key: s.login,
                name: <Dev login={s.login} avatar={s.avatar_url} size="size-4" />,
                s,
              }))}
              selected={author}
              onPick={(k) => set({ author: k === author ? undefined : k })}
            />
          ) : (
            <Breakdown
              title={author ? `Where ${author} merges` : "Repos"}
              rows={d.current
                .filter((s) => s.repo)
                .map((s) => ({
                  key: s.repo ?? "",
                  name: <span className="font-mono">{s.repo}</span>,
                  s,
                }))}
              selected={repo}
              onPick={(k) => set({ repo: k === repo ? undefined : k })}
            />
          )}
          <Llms llms={d.llms} days={days} />
        </div>
        <ExitList workspace={workspace} q={{ repo, author, days, outcome }} showRepo={!repo} />
      </div>
    </div>
  );
}

function RailButton({
  label,
  count,
  selected,
  onClick,
}: {
  label: React.ReactNode;
  count: number;
  selected: boolean;
  onClick: () => void;
}) {
  return (
    <button
      type="button"
      onClick={onClick}
      className={`flex w-full items-center justify-between gap-3 rounded-md px-2 py-1.5 text-left ${selected ? "bg-surface shadow-xs ring-1 ring-line" : "hover:bg-sunken"}`}
    >
      <span className="min-w-0 truncate">{label}</span>
      <span className="text-xs text-faint tabular-nums">{count}</span>
    </button>
  );
}

/** The other dimension as bars of merges, eject rate and median beside. */
function Breakdown({
  title,
  rows,
  selected,
  onPick,
}: {
  title: string;
  rows: { key: string; name: React.ReactNode; s: Slice }[];
  selected?: string;
  onPick: (key: string) => void;
}) {
  const max = Math.max(1, ...rows.map((r) => r.s.merged));
  return (
    <div className="mt-5">
      <p className="mb-2 text-xs font-medium text-muted">{title}</p>
      <ul className="text-xs">
        {rows
          .slice()
          .sort((x, y) => y.s.merged - x.s.merged)
          .map(({ key, name, s }) => (
            <li key={key}>
              <button
                type="button"
                onClick={() => onPick(key)}
                className={`grid w-full grid-cols-[150px_1fr_50px_50px] items-center gap-3 rounded px-1 py-1 text-left hover:bg-sunken ${key === selected ? "bg-sunken" : ""}`}
              >
                <span className="min-w-0 truncate">{name}</span>
                <span className="flex items-center gap-2">
                  <span
                    className="h-2 rounded-sm bg-fg/70"
                    style={{ width: `${(s.merged / max) * 100}%` }}
                  />
                  <span className="tabular-nums text-muted">{s.merged}</span>
                </span>
                <span className="text-right tabular-nums text-muted" title="Eject rate">
                  {pctText(ejectRate(s))}
                </span>
                <span className="text-right tabular-nums text-muted" title="Median time in queue">
                  {s.p50_secs === null ? "–" : duration(s.p50_secs)}
                </span>
              </button>
            </li>
          ))}
      </ul>
    </div>
  );
}

// ---------------------------------------------------------------- C: leaderboard + drawer

type SortKey = "merged" | "eject" | "p50" | "cost" | "permerge";

/** No rail and no single pane: one wide, sortable table grouped by developer or repo, the
 * workspace's numbers in a strip above. A row opens a drawer with that slice's metrics and PRs.
 * The full PR log is its own view (in the real thing, its own top-level tab). */
function VariantC({ workspace }: { workspace: number }) {
  const { by = "developers", sort = "merged", view, repo, author, days, set } = useProtoSearch();
  const [bots, setBots] = useState(false);
  const stats = useStats(workspace, { days });
  if (!stats.data) return null;
  const d = stats.data;
  const all = d.current.at(-1);
  const allPrev = d.previous.at(-1);
  const rows =
    by === "developers"
      ? d.developers
          .filter((s) => bots || !isBot(s.login))
          .map((s) => ({ key: s.login, name: <Dev login={s.login} avatar={s.avatar_url} />, s }))
      : d.current
          .filter((s) => s.repo)
          .map((s) => ({
            key: s.repo ?? "",
            name: <span className="font-mono">{s.repo}</span>,
            s,
          }));
  const value: Record<SortKey, (s: Slice) => number> = {
    merged: (s) => -s.merged,
    eject: (s) => -(ejectRate(s) ?? -1),
    p50: (s) => -(s.p50_secs ?? -1),
    cost: (s) => -s.llm_cost_usd,
    permerge: (s) => -(s.merged ? s.llm_cost_usd / s.merged : -1),
  };
  rows.sort((x, y) => value[sort as SortKey](x.s) - value[sort as SortKey](y.s));
  const maxMerged = Math.max(1, ...rows.map((r) => r.s.merged));
  const open = by === "developers" ? author : repo;
  const close = () => set({ author: undefined, repo: undefined });
  if (view === "log")
    return (
      <div className="h-full overflow-y-auto bg-surface">
        <div className="flex items-center justify-between border-b border-line px-4 py-3 sm:px-6">
          <button
            type="button"
            className="text-sm text-muted hover:text-fg"
            onClick={() => set({ view: undefined })}
          >
            ← Leaderboard
          </button>
          <div className="flex gap-3">
            <OutcomePicker />
            <WindowPicker />
          </div>
        </div>
        <ExitList workspace={workspace} q={{ days }} showRepo />
      </div>
    );
  return (
    <div className="relative h-full overflow-y-auto bg-surface">
      <div className="flex flex-wrap items-center gap-x-6 gap-y-1 border-b border-line bg-page px-4 py-2 text-sm sm:px-6">
        <Kpi
          label="Merged"
          value={String(all?.merged ?? 0)}
          sub={`${(all?.merged ?? 0) - (allPrev?.merged ?? 0) >= 0 ? "+" : ""}${(all?.merged ?? 0) - (allPrev?.merged ?? 0)}`}
        />
        <Kpi label="Eject rate" value={pctText(ejectRate(all))} />
        <Kpi label="Median" value={all?.p50_secs == null ? "–" : duration(all.p50_secs)} />
        <Kpi label="p95" value={all?.p95_secs == null ? "–" : duration(all.p95_secs)} />
        <Kpi label="LLM spend" value={usd(all?.llm_cost_usd ?? 0)} />
        <span className="ml-auto flex items-center gap-3">
          <button
            type="button"
            className="text-muted hover:text-fg"
            onClick={() => set({ view: "log" })}
          >
            PR log →
          </button>
          <WindowPicker />
        </span>
      </div>
      <div className="px-4 py-4 sm:px-6">
        <div className="mb-3 flex flex-wrap items-center gap-3">
          <Segmented
            name="by"
            label="Group by"
            options={["developers", "repos"]}
            value={by}
            text={(o) => (o === "repos" ? "By repo" : "By developer")}
            onChange={(by) => set({ by, author: undefined, repo: undefined })}
          />
          {by === "developers" && (
            <label className="flex items-center gap-1.5 text-xs text-muted">
              <input type="checkbox" checked={bots} onChange={(e) => setBots(e.target.checked)} />
              Include bots
            </label>
          )}
        </div>
        <table className="w-full text-sm">
          <thead className="text-xs text-faint">
            <tr>
              <th className="w-8 py-2 text-left font-normal">#</th>
              <th className="py-2 text-left font-normal">
                {by === "repos" ? "Repo" : "Developer"}
              </th>
              <Th sort={sort} onSort={(sort) => set({ sort })} k="merged">
                Merged
              </Th>
              <Th sort={sort} onSort={(sort) => set({ sort })} k="eject">
                Eject rate
              </Th>
              <Th sort={sort} onSort={(sort) => set({ sort })} k="p50">
                Median
              </Th>
              <Th sort={sort} onSort={(sort) => set({ sort })} k="cost">
                LLM spend
              </Th>
              <Th sort={sort} onSort={(sort) => set({ sort })} k="permerge">
                Per merge
              </Th>
            </tr>
          </thead>
          <tbody className="tabular-nums">
            {rows.map(({ key, name, s }, i) => {
              const rate = ejectRate(s);
              return (
                <tr
                  key={key}
                  onClick={() => set(by === "developers" ? { author: key } : { repo: key })}
                  className={`cursor-pointer border-t border-line-soft hover:bg-sunken ${key === open ? "bg-sunken" : ""}`}
                >
                  <td className="py-2 text-faint">{i + 1}</td>
                  <td className="py-2">{name}</td>
                  <td className="py-2 text-right">
                    <span className="inline-flex w-36 items-center justify-end gap-2">
                      <span
                        className="h-1.5 rounded-sm bg-fg/60"
                        style={{ width: `${(s.merged / maxMerged) * 100}%` }}
                      />
                      {s.merged}
                    </span>
                  </td>
                  <td
                    className={`py-2 text-right ${rate !== null && rate > 0.2 ? "text-bad" : ""}`}
                  >
                    {pctText(rate)}
                  </td>
                  <td className="py-2 text-right">
                    {s.p50_secs === null ? "–" : duration(s.p50_secs)}
                  </td>
                  <td className="py-2 text-right">{usd(s.llm_cost_usd)}</td>
                  <td className="py-2 text-right text-muted">{perMerge(s)}</td>
                </tr>
              );
            })}
          </tbody>
        </table>
      </div>
      {open && (
        <>
          <button
            type="button"
            aria-label="Close"
            className="fixed inset-0 z-20 bg-black/20"
            onClick={close}
          />
          <aside className="fixed top-0 right-0 bottom-0 z-30 w-full max-w-xl overflow-y-auto border-l border-line bg-surface shadow-xl">
            <Drawer workspace={workspace} by={by} id={open} days={days} onClose={close} />
          </aside>
        </>
      )}
    </div>
  );
}

function Th({
  k,
  sort,
  onSort,
  children,
}: {
  k: SortKey;
  sort: string;
  onSort: (k: SortKey) => void;
  children: React.ReactNode;
}) {
  return (
    <th className="py-2 text-right font-normal">
      <button
        type="button"
        onClick={() => onSort(k)}
        className={sort === k ? "font-medium text-fg" : "hover:text-fg"}
      >
        {children} {sort === k && "↓"}
      </button>
    </th>
  );
}

function Kpi({ label, value, sub }: { label: string; value: string; sub?: string }) {
  return (
    <span className="flex items-baseline gap-1.5">
      <span className="text-xs text-muted">{label}</span>
      <span className="font-semibold tabular-nums">{value}</span>
      {sub && <span className="text-xs text-faint tabular-nums">{sub}</span>}
    </span>
  );
}

function Drawer({
  workspace,
  by,
  id,
  days,
  onClose,
}: {
  workspace: number;
  by: string;
  id: string;
  days: number;
  onClose: () => void;
}) {
  const q = by === "developers" ? { author: id, days } : { repo: id, days };
  const stats = useStats(workspace, q);
  const d = stats.data;
  const cur = by === "repos" ? d?.current.find((s) => s.repo === id) : d?.current.at(-1);
  const prev = by === "repos" ? d?.previous.find((s) => s.repo === id) : d?.previous.at(-1);
  const dev = d?.developers.find((x) => x.login === id);
  const total = d?.steps.reduce((t, s) => t + s.mean_secs, 0) ?? 0;
  return (
    <>
      <div className="sticky top-0 flex items-center justify-between border-b border-line bg-surface px-5 py-3">
        {by === "developers" && dev ? (
          <Dev login={dev.login} avatar={dev.avatar_url} size="size-8" />
        ) : (
          <span className="font-mono font-medium">{id}</span>
        )}
        <button type="button" onClick={onClose} className="text-muted hover:text-fg">
          ✕
        </button>
      </div>
      {d && (
        <div className="px-5 py-4">
          <Stats current={cur} previous={prev} mergedByDay={d.merged_by_day} days={days} />
          {d.steps.length > 0 && (
            <div className="mt-5">
              <p className="mb-2 text-xs font-medium text-muted">
                Where the time goes · {duration(total)} on average
              </p>
              <Bar
                height="h-3"
                label="Steps"
                parts={d.steps.map((s) => ({
                  step: s.step,
                  secs: s.mean_secs,
                  title: `${stepInfo[s.step][0]} ${duration(s.mean_secs)}`,
                }))}
              />
            </div>
          )}
          <Llms llms={d.llms} days={days} />
          {by === "developers" && <p className="mt-5 mb-1 text-xs font-medium text-muted">Repos</p>}
          {by === "developers" && (
            <ul className="flex flex-wrap gap-2 text-xs">
              {d.current
                .filter((s) => s.repo)
                .map((s) => (
                  <li key={s.repo} className="rounded bg-sunken px-2 py-0.5">
                    <span className="font-mono">{s.repo}</span>{" "}
                    <span className="text-muted">{s.merged}</span>
                  </li>
                ))}
            </ul>
          )}
        </div>
      )}
      <div className="border-t border-line">
        <ExitList workspace={workspace} q={q} showRepo={by === "developers"} />
      </div>
    </>
  );
}
