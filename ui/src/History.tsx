import { keepPreviousData, useInfiniteQuery, useQuery } from "@tanstack/react-query";
import { useNavigate, useSearch } from "@tanstack/react-router";
import { useState } from "react";
import { fetchExits, fetchHistoryStats } from "./api";
import type { DeveloperStatsView } from "./api/DeveloperStatsView";
import type { ExitView } from "./api/ExitView";
import type { LlmRunsView } from "./api/LlmRunsView";
import type { LlmStatsView } from "./api/LlmStatsView";
import type { OutcomeView } from "./api/OutcomeView";
import type { StatsView } from "./api/StatsView";
import type { StepStatsView } from "./api/StepStatsView";
import type { StepTimeView } from "./api/StepTimeView";
import type { StepView } from "./api/StepView";
import { Avatar, duration, llmName, RepoIcon, secondary, timeAgo, usd } from "./ui";

export const WINDOWS = [7, 30, 90] as const;
export type Window = (typeof WINDOWS)[number];
export const OUTCOMES = ["merged", "ejected", "removed"] as const;
/** What the side lists: picking one scopes the page to it. */
export const LISTS = ["repos", "developers"] as const;
export type List = (typeof LISTS)[number];

/** Two panes: the repos or the developers, and the metrics and the PRs that left the queue of
 * the one picked. A repo's developers, or a developer's repos, break down below the metrics:
 * picking one there narrows to both. A narrow screen lists the side in a row above. */
export function History({ workspace }: { workspace: number }) {
  const {
    list = "repos",
    repo,
    author,
    days = 30,
    outcome,
    pr,
  } = useSearch({
    from: "/$account/history",
  });
  const navigate = useNavigate({ from: "/$account/history" });
  // The side lists every repo or developer, whatever the scope.
  const side = useQuery({
    queryKey: ["history-stats", workspace, undefined, undefined, days],
    queryFn: () => fetchHistoryStats(workspace, { days }),
    refetchInterval: 10_000,
  });
  // A new scope keeps the last one on screen until its own data loads, instead of blanking the page.
  const stats = useQuery({
    queryKey: ["history-stats", workspace, repo, author, days],
    queryFn: () => fetchHistoryStats(workspace, { repo, author, days }),
    placeholderData: keepPreviousData,
    refetchInterval: 10_000,
  });
  const exits = useInfiniteQuery({
    queryKey: ["history", workspace, repo, author, days, outcome, pr],
    queryFn: ({ pageParam }) =>
      fetchExits(workspace, { repo, author, days, outcome, pr, before: pageParam ?? undefined }),
    initialPageParam: null as number | null,
    getNextPageParam: (page) => page.next,
    placeholderData: keepPreviousData,
    refetchInterval: 10_000,
  });

  if (side.isError || stats.isError || exits.isError)
    return <p className="p-6 text-bad">Could not load the history.</p>;
  if (!side.data || !stats.data) return null;

  const { current, previous, developers, merged_by_day, steps, llms } = stats.data;
  const of = (all: StatsView[]) => all.find((s) => s.repo === (repo ?? null));
  const rows = exits.data?.pages.flatMap((p) => p.exits) ?? [];
  const scope = (s: { repo?: string; author?: string }) =>
    navigate({ search: (prev) => ({ ...prev, ...s, pr: undefined }) });
  const byRepo = list === "repos";
  // The scope the side did not pick: one picked in the breakdown.
  const narrowed = byRepo ? author : repo;
  return (
    <div className="flex h-full flex-col md:grid md:grid-cols-[220px_1fr]">
      <div className="shrink-0 border-line p-2 max-md:flex max-md:items-center max-md:gap-1 max-md:overflow-x-auto max-md:border-b md:overflow-y-auto md:border-r">
        <div className="shrink-0 px-1 pb-1 md:pt-1">
          <Segmented
            name="list"
            label="List"
            fill
            options={LISTS}
            value={list}
            text={(l) => (l === "repos" ? "Repos" : "Developers")}
            onChange={(list) =>
              navigate({
                search: (s) => ({ ...s, list, repo: undefined, author: undefined }),
              })
            }
          />
        </div>
        {byRepo ? (
          <>
            <SideButton
              label={<All label="All repos" />}
              count={side.data.current.at(-1)?.merged}
              days={days}
              selected={!repo}
              onClick={() => scope({ repo: undefined })}
            />
            {side.data.current
              .filter((s) => s.repo !== null)
              .map((s) => (
                <SideButton
                  key={s.repo}
                  label={<Repo repo={s.repo ?? ""} short />}
                  title={s.repo ?? ""}
                  count={s.merged}
                  days={days}
                  selected={s.repo === repo}
                  onClick={() => scope({ repo: s.repo ?? undefined })}
                />
              ))}
          </>
        ) : (
          <>
            <SideButton
              label={<All label="All developers" />}
              count={side.data.current.at(-1)?.merged}
              days={days}
              selected={!author}
              onClick={() => scope({ author: undefined })}
            />
            {byMerged(side.data.developers).map((d) => (
              <SideButton
                key={d.author.login}
                label={<Developer stats={d} />}
                title={d.author.login}
                count={d.merged}
                days={days}
                selected={d.author.login === author}
                onClick={() => scope({ author: d.author.login })}
              />
            ))}
          </>
        )}
      </div>
      <div
        className={`min-h-0 flex-1 overflow-y-auto bg-surface transition-opacity ${stats.isPlaceholderData ? "opacity-60" : ""}`}
      >
        <div className="border-b border-line px-4 py-4 sm:px-6">
          <div className="flex flex-wrap items-center justify-between gap-x-4 gap-y-2">
            <p className="flex min-w-0 flex-wrap items-baseline gap-x-2 font-medium">
              {[author, repo].filter(Boolean).join(" in ") ||
                (byRepo ? "All repos" : "All developers")}
              {narrowed && (
                <button
                  type="button"
                  className="text-xs font-normal text-muted hover:text-fg"
                  onClick={() => scope(byRepo ? { author: undefined } : { repo: undefined })}
                >
                  Clear {narrowed}
                </button>
              )}
            </p>
            <div className="flex flex-wrap items-center gap-3">
              <Segmented
                name="outcome"
                label="Outcome"
                options={[undefined, ...OUTCOMES]}
                value={outcome}
                text={(o) => (o ? o[0].toUpperCase() + o.slice(1) : "All")}
                onChange={(outcome) => navigate({ search: (s) => ({ ...s, outcome }) })}
              />
              <Segmented
                name="window"
                label="Window"
                options={WINDOWS}
                value={days}
                text={(d) => `${d}d`}
                onChange={(days) => navigate({ search: (s) => ({ ...s, days }) })}
              />
            </div>
          </div>
          <Stats
            current={of(current)}
            previous={of(previous)}
            mergedByDay={merged_by_day}
            days={days}
          />
          <Steps steps={steps} />
          {byRepo ? (
            <Breakdown
              title={repo ? "Developers in this repo" : "Developers"}
              rows={byMerged(developers).map((d) => ({
                key: d.author.login,
                name: <Developer stats={d} />,
                stats: d,
              }))}
              selected={author}
              onPick={(login) => scope({ author: login === author ? undefined : login })}
            />
          ) : (
            <Breakdown
              title={author ? `Where ${author} merges` : "Repos"}
              rows={byMerged(current.filter((s) => s.repo !== null)).map((s) => ({
                key: s.repo ?? "",
                name: <Repo repo={s.repo ?? ""} />,
                stats: s,
              }))}
              selected={repo}
              onPick={(r) => scope({ repo: r === repo ? undefined : r })}
            />
          )}
          <Llms llms={llms} days={days} />
        </div>
        {pr !== undefined && repo && (
          <p className="flex items-baseline gap-2 border-b border-line-soft px-4 py-2 text-sm sm:px-6">
            Only #{pr}, from any time
            <button
              type="button"
              className="text-xs text-muted hover:text-fg"
              onClick={() => navigate({ search: (s) => ({ ...s, pr: undefined }) })}
            >
              Show every PR
            </button>
          </p>
        )}
        {exits.data && rows.length === 0 && (
          <p className="p-6 text-faint">
            {pr !== undefined && repo
              ? `#${pr} has not left the queue.`
              : `No PR ${outcome ? `was ${outcome}` : "left the queue"} in the last ${days} days.`}
          </p>
        )}
        <ol>
          {rows.map((e) => (
            <li key={e.id}>
              <Row exit={e} showRepo={!repo} />
            </li>
          ))}
        </ol>
        {exits.hasNextPage && (
          <div className="p-4 text-center">
            <button
              type="button"
              className={secondary}
              disabled={exits.isFetchingNextPage}
              onClick={() => exits.fetchNextPage()}
            >
              {exits.isFetchingNextPage ? "Loading…" : "Load more"}
            </button>
          </div>
        )}
      </div>
    </div>
  );
}

const byMerged = <T extends { merged: number }>(all: T[]) =>
  [...all].sort((a, b) => b.merged - a.merged);

/** Lines up with the icons and avatars of the rows below it. */
function All({ label }: { label: string }) {
  return (
    <span className="flex min-w-0 items-center gap-2">
      <span
        aria-hidden
        className="grid size-4 shrink-0 grid-cols-2 gap-px rounded bg-sunken p-[3px] ring-1 ring-line"
      >
        <span className="rounded-[1px] bg-faint" />
        <span className="rounded-[1px] bg-faint" />
        <span className="rounded-[1px] bg-faint" />
        <span className="rounded-[1px] bg-faint" />
      </span>
      <span className="truncate">{label}</span>
    </span>
  );
}

function Repo({ repo, short }: { repo: string; short?: boolean }) {
  return (
    <span className="flex min-w-0 items-center gap-2">
      <RepoIcon repo={repo} size="size-4" />
      <span className="truncate">{short ? repo.split("/")[1] : repo}</span>
    </span>
  );
}

function Developer({ stats: d }: { stats: DeveloperStatsView }) {
  return (
    <span className="flex min-w-0 items-center gap-2">
      <Avatar src={d.author.avatar_url} alt={d.author.login} size="size-4" />
      <span className="truncate">{d.author.login.replace(/\[bot\]$/, "")}</span>
      {d.bot && <span className="text-[10px] text-faint uppercase">Bot</span>}
    </span>
  );
}

function Segmented<T extends string | number | undefined>({
  name,
  label,
  options,
  value,
  text,
  onChange,
  fill,
}: {
  name: string;
  label: string;
  options: readonly T[];
  value: T;
  text: (option: T) => string;
  onChange: (option: T) => void;
  /** Stretch the options evenly across the container's width. */
  fill?: boolean;
}) {
  return (
    <fieldset className="flex rounded-md bg-sunken p-0.5 text-xs">
      <legend className="sr-only">{label}</legend>
      {options.map((o) => (
        <label
          key={String(o)}
          className={`${fill ? "flex-1 text-center " : ""}cursor-pointer rounded px-2 py-1 text-muted hover:text-fg has-checked:bg-surface has-checked:text-fg has-checked:shadow-xs has-checked:ring-1 has-checked:ring-line has-focus-visible:ring-2 has-focus-visible:ring-fg/30`}
        >
          <input
            type="radio"
            name={name}
            checked={o === value}
            onChange={() => onChange(o)}
            className="sr-only"
          />
          {text(o)}
        </label>
      ))}
    </fieldset>
  );
}

function SideButton({
  label,
  title,
  count,
  days,
  selected,
  onClick,
}: {
  label: React.ReactNode;
  title?: string;
  count?: number;
  days: number;
  selected: boolean;
  onClick: () => void;
}) {
  return (
    <button
      type="button"
      onClick={onClick}
      title={title}
      className={`flex w-full items-center justify-between gap-3 rounded-md px-2 py-1.5 text-left max-md:w-auto max-md:shrink-0 ${
        selected ? "bg-surface shadow-xs ring-1 ring-line" : "hover:bg-sunken"
      }`}
    >
      <span className="min-w-0 truncate">{label}</span>
      <span className="text-xs text-faint tabular-nums" title={`Merged in the last ${days} days`}>
        {count ?? 0}
      </span>
    </button>
  );
}

type Counts = Pick<StatsView, "merged" | "ejected" | "p50_secs">;

/** Ejected of the PRs the queue tried: `null` before it tried any. */
function ejectRate(s?: Counts) {
  const tried = (s?.merged ?? 0) + (s?.ejected ?? 0);
  return tried ? (s?.ejected ?? 0) / tried : null;
}

function Stats({
  current,
  previous,
  mergedByDay,
  days,
}: {
  current?: StatsView;
  previous?: StatsView;
  mergedByDay: number[];
  days: number;
}) {
  const vs = `vs the ${days} days before`;
  const rate = ejectRate(current);
  const rateBefore = ejectRate(previous);
  const p50 = current?.p50_secs ?? null;
  const p95 = current?.p95_secs ?? null;
  return (
    <dl className="mt-4 grid grid-cols-2 gap-3 lg:grid-cols-4">
      <Tile label="Merged" value={String(current?.merged ?? 0)}>
        <Delta
          change={(current?.merged ?? 0) - (previous?.merged ?? 0)}
          text={(d) => String(Math.abs(d))}
          good="up"
          vs={vs}
        />
        <Sparkline counts={mergedByDay} />
      </Tile>
      <Tile label="Eject rate" value={rate === null ? "–" : `${Math.round(rate * 100)}%`}>
        {rate !== null && rateBefore !== null && (
          <Delta
            change={Math.round((rate - rateBefore) * 100)}
            text={(d) => `${Math.abs(d)} pts`}
            good="down"
            vs={vs}
          />
        )}
      </Tile>
      <Tile label="Median time in queue" value={p50 === null ? "–" : duration(p50)}>
        <TimeDelta now={p50} before={previous?.p50_secs ?? null} vs={vs} />
      </Tile>
      <Tile label="95th percentile" value={p95 === null ? "–" : duration(p95)}>
        <TimeDelta now={p95} before={previous?.p95_secs ?? null} vs={vs} />
      </Tile>
    </dl>
  );
}

function Tile({
  label,
  value,
  children,
}: {
  label: string;
  value: string;
  children?: React.ReactNode;
}) {
  return (
    <div className="rounded-lg border border-line px-4 py-3">
      <dt className="text-xs text-muted">{label}</dt>
      <dd className="mt-1 text-2xl font-semibold tracking-tight">{value}</dd>
      {children}
    </div>
  );
}

/** A change from the window before: green when it went the way that is good, red otherwise. */
function Delta({
  change,
  text,
  good,
  vs,
}: {
  change: number;
  text: (change: number) => string;
  good: "up" | "down";
  vs: string;
}) {
  if (change === 0) return <dd className="text-xs text-faint">No change {vs}</dd>;
  const better = change > 0 === (good === "up");
  return (
    <dd className="text-xs text-faint">
      <span className={better ? "text-ok" : "text-bad"}>
        {change > 0 ? "▲" : "▼"} {text(change)}
      </span>{" "}
      {vs}
    </dd>
  );
}

function TimeDelta({ now, before, vs }: { now: number | null; before: number | null; vs: string }) {
  if (now === null || before === null || before === 0) return null;
  return (
    <Delta
      change={Math.round(((now - before) / before) * 100)}
      text={(d) => `${Math.abs(d)}%`}
      good="down"
      vs={vs}
    />
  );
}

const SPARK_W = 120;
const SPARK_H = 24;

/** Merges per UTC day, today last and marked. Hover, tap, or drag for a day's count. */
function Sparkline({ counts }: { counts: number[] }) {
  const [hover, setHover] = useState<number | null>(null);
  if (counts.length < 2) return null;
  const max = Math.max(1, ...counts);
  const step = SPARK_W / (counts.length - 1);
  const x = (i: number) => i * step;
  const y = (n: number) => SPARK_H - 2 - (n / max) * (SPARK_H - 4);
  const points = counts.map((n, i) => `${x(i)},${y(n)}`).join(" ");
  const last = counts.length - 1;
  // A day tapped in a longer window may be past the end of this one.
  const shown = Math.min(hover ?? last, last);
  const ago = last - shown;
  const pick = (el: Element, clientX: number) => {
    const r = el.getBoundingClientRect();
    const i = Math.round(((clientX - r.left) / r.width) * last);
    setHover(Math.min(last, Math.max(0, i)));
  };
  return (
    <dd className="mt-2 flex items-center gap-2">
      <svg
        viewBox={`-3 0 ${SPARK_W + 6} ${SPARK_H}`}
        className="h-6 w-30 min-w-0 touch-pan-y overflow-visible"
        role="img"
        aria-label={`Merged per day: ${counts.join(", ")}`}
        onPointerDown={(ev) => pick(ev.currentTarget, ev.clientX)}
        onPointerMove={(ev) => pick(ev.currentTarget, ev.clientX)}
        // A tapped day stays until the next tap: a finger leaves as it lifts.
        onPointerLeave={(ev) => ev.pointerType === "mouse" && setHover(null)}
      >
        <polyline
          points={points}
          fill="none"
          className="stroke-faint"
          strokeWidth="1.5"
          strokeLinejoin="round"
          strokeLinecap="round"
        />
        <circle cx={x(shown)} cy={y(counts[shown])} r="2.5" className="fill-fg" />
      </svg>
      <span className="shrink-0 text-xs text-faint tabular-nums">
        {counts[shown]} {ago === 0 ? "today" : ago === 1 ? "yesterday" : `${ago}d ago`}
      </span>
    </dd>
  );
}

const outcomeBadge: Record<OutcomeView, [string, string]> = {
  merged: [
    "Merged",
    "bg-green-50 text-green-800 ring-green-600/20 dark:bg-green-400/10 dark:text-green-300 dark:ring-green-400/30",
  ],
  ejected: [
    "Ejected",
    "bg-red-50 text-red-800 ring-red-600/20 dark:bg-red-400/10 dark:text-red-300 dark:ring-red-400/30",
  ],
  removed: ["Removed", "bg-sunken text-fg-2 ring-neutral-500/20"],
};

function Row({ exit: e, showRepo }: { exit: ExitView; showRepo: boolean }) {
  const [label, style] = outcomeBadge[e.outcome];
  const url = e.url || `https://github.com/${e.repo}/pull/${e.number}`;
  const queued =
    e.enqueued_at && (new Date(e.at).getTime() - new Date(e.enqueued_at).getTime()) / 1000;
  return (
    <div className="flex gap-3 border-b border-line-soft px-4 py-3 sm:px-6">
      <span className="min-w-0 flex-1">
        <span className="flex items-start justify-between gap-3">
          <a
            href={url}
            target="_blank"
            rel="noreferrer"
            className="truncate font-medium hover:underline"
          >
            {e.title || `Pull request #${e.number}`}
          </a>
          <span
            className={`shrink-0 rounded-md px-1.5 py-0.5 text-xs font-medium ring-1 ring-inset ${style}`}
          >
            {label}
          </span>
        </span>
        <span className="mt-1 flex flex-wrap items-center gap-x-2 gap-y-1 text-xs text-muted">
          {showRepo && <span>{e.repo}</span>}
          <span className="tabular-nums">#{e.number}</span>
          {e.fork && <span title="From a fork">Fork</span>}
          {e.author && (
            <span className="flex items-center gap-1">
              <Avatar src={e.author.avatar_url} alt={e.author.login} size="size-4" />
              {e.author.login}
            </span>
          )}
          {typeof queued === "number" && (
            <span title="Time in the queue">{duration(queued)} in queue</span>
          )}
          <time className="ml-auto text-faint" title={e.at}>
            {timeAgo(e.at)}
          </time>
        </span>
        <Detail exit={e} />
        <StepBar steps={e.steps} />
      </span>
    </div>
  );
}

function Detail({ exit: e }: { exit: ExitView }) {
  if (e.outcome === "merged")
    return (
      <a
        href={`https://github.com/${e.repo}/commit/${e.detail}`}
        target="_blank"
        rel="noreferrer"
        className="mt-1 block font-mono text-xs text-muted hover:text-fg"
      >
        {e.detail.slice(0, 7)} → {e.trunk}
      </a>
    );
  if (e.outcome === "ejected")
    return <span className="mt-1 block text-xs break-words text-bad">{e.detail}</span>;
  return null;
}

const stepInfo: Record<StepView, [string, string]> = {
  waiting: ["Waiting", "var(--step-1)"],
  held: ["Held", "var(--step-2)"],
  rebasing: ["Rebasing", "var(--step-3)"],
  checks: ["Checks", "var(--step-4)"],
  fixing: ["Fixing", "var(--step-5)"],
};

/** The steps laid end to end, each as wide as its share. Hover one for its name and time. */
function Bar({
  parts,
  height,
  label,
}: {
  parts: { step: StepView; secs: number; title: string }[];
  height: string;
  label: string;
}) {
  const total = parts.reduce((sum, p) => sum + p.secs, 0);
  if (total === 0) return null;
  return (
    // The gaps between steps are the surface showing through.
    <span className={`flex gap-0.5 ${height}`} role="img" aria-label={label}>
      {parts
        .filter((p) => p.secs > 0)
        .map((p) => (
          <span
            key={p.step}
            title={p.title}
            className="min-w-0.5 first:rounded-l last:rounded-r hover:opacity-80"
            style={{ flexGrow: p.secs, background: stepInfo[p.step][1] }}
          />
        ))}
    </span>
  );
}

/** The other side of the scope: merges as bars, then the eject rate and the median time in the
 * queue. Picking a row narrows to it, and picking it again clears it. */
function Breakdown({
  title,
  rows,
  selected,
  onPick,
}: {
  title: string;
  rows: { key: string; name: React.ReactNode; stats: Counts }[];
  selected?: string;
  onPick: (key: string) => void;
}) {
  if (rows.length === 0) return null;
  const max = Math.max(1, ...rows.map((r) => r.stats.merged));
  return (
    <div className="mt-5">
      <div className="mb-2 flex items-baseline justify-between gap-3">
        <p className="text-xs font-medium text-muted">{title}</p>
        <p className="text-xs text-faint max-sm:hidden">Merged · eject rate · median</p>
      </div>
      <ul className="text-xs">
        {rows.map(({ key, name, stats: s }) => {
          const rate = ejectRate(s);
          return (
            <li key={key}>
              <button
                type="button"
                onClick={() => onPick(key)}
                aria-pressed={key === selected}
                className={`grid w-full grid-cols-[minmax(0,10rem)_1fr_3rem_3rem] items-center gap-3 rounded px-1 py-1 text-left ${
                  key === selected ? "bg-sunken ring-1 ring-line" : "hover:bg-sunken"
                }`}
              >
                <span className="min-w-0 truncate">{name}</span>
                <span className="flex min-w-0 items-center gap-2">
                  <span
                    className="h-2 rounded-sm bg-faint"
                    style={{ width: `${(s.merged / max) * 100}%` }}
                  />
                  <span className="text-muted tabular-nums">{s.merged}</span>
                </span>
                <span className="text-right text-muted tabular-nums" title="Eject rate">
                  {rate === null ? "–" : `${Math.round(rate * 100)}%`}
                </span>
                <span className="text-right text-muted tabular-nums" title="Median time in queue">
                  {s.p50_secs === null ? "–" : duration(s.p50_secs)}
                </span>
              </button>
            </li>
          );
        })}
      </ul>
    </div>
  );
}

/** Where the merged PRs' time went: the average of each step end to end, then each step's
 * numbers, which double as the legend. */
function Steps({ steps }: { steps: StepStatsView[] }) {
  if (steps.length === 0) return null;
  const total = steps.reduce((sum, s) => sum + s.mean_secs, 0);
  const label = steps.map((s) => `${stepInfo[s.step][0]} ${duration(s.mean_secs)}`).join(", ");
  return (
    <div className="mt-5">
      <div className="mb-2 flex flex-wrap items-baseline justify-between gap-x-3">
        <p className="text-xs font-medium text-muted">Where the time goes</p>
        <p className="text-xs text-faint">{duration(total)} on average from label to merge</p>
      </div>
      <Bar
        height="h-3"
        label={`Average time in each step: ${label}`}
        parts={steps.map((s) => ({
          step: s.step,
          secs: s.mean_secs,
          title: `${stepInfo[s.step][0]}: ${duration(s.mean_secs)} on average, ${duration(s.p50_secs)} median, ${duration(s.p95_secs)} at the 95th percentile`,
        }))}
      />
      <table className="mt-3 w-full text-xs">
        <thead className="text-faint">
          <tr>
            <th className="py-1 text-left font-normal">Step</th>
            <th className="py-1 text-right font-normal">Average</th>
            <th className="py-1 text-right font-normal">Share</th>
            <th className="py-1 text-right font-normal">Median</th>
            <th className="py-1 text-right font-normal">95th percentile</th>
          </tr>
        </thead>
        <tbody className="tabular-nums">
          {steps.map((s) => (
            <tr key={s.step} className="border-t border-line-soft">
              <td className="py-1">
                <span className="flex items-center gap-2">
                  <span
                    className="size-2.5 rounded-sm"
                    style={{ background: stepInfo[s.step][1] }}
                  />
                  {stepInfo[s.step][0]}
                </span>
              </td>
              <td className="py-1 text-right">{duration(s.mean_secs)}</td>
              <td className="py-1 text-right text-muted">
                {total ? `${Math.round((s.mean_secs / total) * 100)}%` : "–"}
              </td>
              <td className="py-1 text-right text-muted">{duration(s.p50_secs)}</td>
              <td className="py-1 text-right text-muted">{duration(s.p95_secs)}</td>
            </tr>
          ))}
        </tbody>
      </table>
    </div>
  );
}

/** What the agents cost over the window, by model and effort, split into resolving rebase
 * conflicts, fixing failed checks, and reviewing PRs. */
function Llms({ llms, days }: { llms: LlmStatsView[]; days: number }) {
  if (llms.length === 0) return null;
  const sum = (runs: (l: LlmStatsView) => LlmRunsView): LlmRunsView => ({
    runs: llms.reduce((t, l) => t + runs(l).runs, 0),
    failed: llms.reduce((t, l) => t + runs(l).failed, 0),
    turns: llms.reduce((t, l) => t + runs(l).turns, 0),
    cost_usd: llms.reduce((t, l) => t + runs(l).cost_usd, 0),
    unpriced: llms.reduce((t, l) => t + runs(l).unpriced, 0),
  });
  const rebase = sum((l) => l.rebase);
  const fix = sum((l) => l.fix);
  const review = sum((l) => l.review);
  return (
    <div className="mt-5">
      <div className="mb-2 flex items-baseline justify-between">
        <p className="text-xs font-medium text-muted">LLM spend</p>
        <p className="text-xs text-faint">
          {cost(all(rebase, fix, review))} over {rebase.runs + fix.runs + review.runs} runs in the
          last {days} days
        </p>
      </div>
      <table className="w-full text-xs">
        <thead className="text-faint">
          <tr>
            <th className="py-1 text-left font-normal">Model</th>
            <th className="py-1 text-right font-normal">Rebasing</th>
            <th className="py-1 text-right font-normal">Fixing</th>
            <th className="py-1 text-right font-normal">Reviewing</th>
            <th className="py-1 text-right font-normal">Total</th>
          </tr>
        </thead>
        <tbody className="tabular-nums">
          {llms.map((l) => (
            <tr key={llmName(l)} className="border-t border-line-soft">
              <td className="py-1 font-mono">{llmName(l)}</td>
              <LlmRuns runs={l.rebase} />
              <LlmRuns runs={l.fix} />
              <LlmRuns runs={l.review} />
              <td className="py-1 text-right font-medium">
                {cost(all(l.rebase, l.fix, l.review))}
              </td>
            </tr>
          ))}
          {llms.length > 1 && (
            <tr className="border-t border-line font-medium">
              <td className="py-1">All models</td>
              <LlmRuns runs={rebase} />
              <LlmRuns runs={fix} />
              <LlmRuns runs={review} />
              <td className="py-1 text-right">{cost(all(rebase, fix, review))}</td>
            </tr>
          )}
        </tbody>
      </table>
    </div>
  );
}

const all = (...rs: LlmRunsView[]): LlmRunsView => ({
  runs: rs.reduce((t, r) => t + r.runs, 0),
  failed: rs.reduce((t, r) => t + r.failed, 0),
  turns: rs.reduce((t, r) => t + r.turns, 0),
  cost_usd: rs.reduce((t, r) => t + r.cost_usd, 0),
  unpriced: rs.reduce((t, r) => t + r.unpriced, 0),
});

/** The known cost: "–" when no run told it, "$1.20+" when only some did, such as Codex's. */
const cost = (r: LlmRunsView) =>
  r.unpriced === r.runs ? "–" : `${usd(r.cost_usd)}${r.unpriced > 0 ? "+" : ""}`;

function LlmRuns({ runs }: { runs: LlmRunsView }) {
  return (
    <td
      className="py-1 text-right"
      title={`${runs.runs} ${runs.runs === 1 ? "run" : "runs"}${runs.failed > 0 ? ` (${runs.failed} failed)` : ""}, ${runs.turns} turns${runs.unpriced > 0 ? `, ${runs.unpriced} without a cost` : ""}`}
    >
      {runs.runs === 0 ? (
        <span className="text-faint">–</span>
      ) : (
        <>
          {cost(runs)} <span className="text-faint">· {runs.runs}×</span>
        </>
      )}
    </td>
  );
}

/** One PR's steps, end to end. Click or tap for each step's time, which a touch cannot hover
 * for. */
function StepBar({ steps }: { steps: StepTimeView[] }) {
  const [open, setOpen] = useState(false);
  const spent = steps.filter((s) => s.secs > 0);
  if (spent.length === 0) return null;
  const label = spent.map((s) => `${stepInfo[s.step][0]} ${duration(s.secs)}`).join(", ");
  return (
    <div className="mt-1">
      <button
        type="button"
        onClick={() => setOpen(!open)}
        aria-expanded={open}
        aria-label="Time in each step"
        className="block w-full cursor-pointer py-1"
      >
        <Bar
          height="h-1.5"
          label={`Time in each step: ${label}`}
          parts={steps.map((s) => ({
            step: s.step,
            secs: s.secs,
            title: `${stepInfo[s.step][0]}: ${duration(s.secs)}`,
          }))}
        />
      </button>
      {open && (
        <ul className="flex flex-wrap gap-x-3 gap-y-1 text-xs text-muted tabular-nums">
          {spent.map((s) => (
            <li key={s.step} className="flex items-center gap-1.5">
              <span className="size-2 rounded-sm" style={{ background: stepInfo[s.step][1] }} />
              {stepInfo[s.step][0]} {duration(s.secs)}
            </li>
          ))}
        </ul>
      )}
    </div>
  );
}
