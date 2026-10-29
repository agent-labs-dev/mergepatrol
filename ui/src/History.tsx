import { useInfiniteQuery, useQuery } from "@tanstack/react-query";
import { useNavigate, useSearch } from "@tanstack/react-router";
import { useState } from "react";
import { fetchExits, fetchHistoryStats } from "./api";
import type { ExitView } from "./api/ExitView";
import type { OutcomeView } from "./api/OutcomeView";
import type { StatsView } from "./api/StatsView";
import type { StepStatsView } from "./api/StepStatsView";
import type { StepTimeView } from "./api/StepTimeView";
import type { StepView } from "./api/StepView";
import { Avatar, duration, secondary, timeAgo } from "./ui";

export const WINDOWS = [7, 30, 90] as const;
export type Window = (typeof WINDOWS)[number];
export const OUTCOMES = ["merged", "ejected", "removed"] as const;

/** Two panes: the repos, and the selected repo's metrics and the PRs that left its queue. */
export function History({ workspace }: { workspace: number }) {
  const { repo, days = 30, outcome } = useSearch({ from: "/$account/history" });
  const navigate = useNavigate({ from: "/$account/history" });
  const stats = useQuery({
    queryKey: ["history-stats", workspace, repo, days],
    queryFn: () => fetchHistoryStats(workspace, { repo, days }),
    refetchInterval: 10_000,
  });
  const exits = useInfiniteQuery({
    queryKey: ["history", workspace, repo, days, outcome],
    queryFn: ({ pageParam }) =>
      fetchExits(workspace, { repo, days, outcome, before: pageParam ?? undefined }),
    initialPageParam: null as number | null,
    getNextPageParam: (page) => page.next,
    refetchInterval: 10_000,
  });

  if (stats.isError || exits.isError)
    return <p className="p-6 text-bad">Could not load the history.</p>;
  if (!stats.data) return null;

  const { current, previous, merged_by_day, steps } = stats.data;
  const repos = current.filter((s) => s.repo !== null);
  const of = (all: StatsView[]) => all.find((s) => s.repo === (repo ?? null));
  const rows = exits.data?.pages.flatMap((p) => p.exits) ?? [];
  const pick = (repo?: string) => navigate({ search: (s) => ({ ...s, repo }) });
  return (
    <div className="grid h-full grid-cols-[220px_1fr]">
      <div className="overflow-y-auto border-r border-line p-2">
        <p className="px-2 py-2 text-xs font-medium text-muted">Repos</p>
        <RepoButton
          label="All repos"
          stats={current.at(-1)}
          days={days}
          selected={!repo}
          onClick={() => pick()}
        />
        {repos.map((s) => (
          <RepoButton
            key={s.repo}
            label={s.repo?.split("/")[1] ?? ""}
            title={s.repo ?? ""}
            stats={s}
            days={days}
            selected={s.repo === repo}
            onClick={() => pick(s.repo ?? undefined)}
          />
        ))}
      </div>
      <div className="overflow-y-auto bg-surface">
        <div className="border-b border-line px-6 py-4">
          <div className="flex items-center justify-between gap-4">
            <p className="font-medium">{repo ?? "All repos"}</p>
            <div className="flex items-center gap-3">
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
        </div>
        {exits.data && rows.length === 0 && (
          <p className="p-6 text-faint">
            No PR {outcome ? `was ${outcome}` : "left the queue"} in the last {days} days.
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

function Segmented<T extends string | number | undefined>({
  name,
  label,
  options,
  value,
  text,
  onChange,
}: {
  name: string;
  label: string;
  options: readonly T[];
  value: T;
  text: (option: T) => string;
  onChange: (option: T) => void;
}) {
  return (
    <fieldset className="flex rounded-md bg-sunken p-0.5 text-xs">
      <legend className="sr-only">{label}</legend>
      {options.map((o) => (
        <label
          key={String(o)}
          className="cursor-pointer rounded px-2 py-1 text-muted hover:text-fg has-checked:bg-surface has-checked:text-fg has-checked:shadow-xs has-checked:ring-1 has-checked:ring-line has-focus-visible:ring-2 has-focus-visible:ring-fg/30"
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

function RepoButton({
  label,
  title,
  stats,
  days,
  selected,
  onClick,
}: {
  label: string;
  title?: string;
  stats?: StatsView;
  days: number;
  selected: boolean;
  onClick: () => void;
}) {
  return (
    <button
      type="button"
      onClick={onClick}
      title={title}
      className={`flex w-full items-center justify-between rounded-md px-2 py-1.5 text-left ${
        selected ? "bg-surface shadow-xs ring-1 ring-line" : "hover:bg-sunken"
      }`}
    >
      <span className="truncate">{label}</span>
      <span className="text-xs text-faint tabular-nums" title={`Merged in the last ${days} days`}>
        {stats?.merged ?? 0}
      </span>
    </button>
  );
}

/** Ejected of the PRs the queue tried: `null` before it tried any. */
function ejectRate(s?: StatsView) {
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
    <dl className="mt-4 grid grid-cols-4 gap-3">
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

/** Merges per UTC day, today last and marked. Hover for a day's count. */
function Sparkline({ counts }: { counts: number[] }) {
  const [hover, setHover] = useState<number | null>(null);
  if (counts.length < 2) return null;
  const max = Math.max(1, ...counts);
  const step = SPARK_W / (counts.length - 1);
  const x = (i: number) => i * step;
  const y = (n: number) => SPARK_H - 2 - (n / max) * (SPARK_H - 4);
  const points = counts.map((n, i) => `${x(i)},${y(n)}`).join(" ");
  const last = counts.length - 1;
  const shown = hover ?? last;
  const ago = last - shown;
  return (
    <dd className="mt-2 flex items-center gap-2">
      <svg
        viewBox={`-3 0 ${SPARK_W + 6} ${SPARK_H}`}
        className="h-6 w-30 overflow-visible"
        role="img"
        aria-label={`Merged per day: ${counts.join(", ")}`}
        onMouseMove={(ev) => {
          const r = ev.currentTarget.getBoundingClientRect();
          const i = Math.round(((ev.clientX - r.left) / r.width) * last);
          setHover(Math.min(last, Math.max(0, i)));
        }}
        onMouseLeave={() => setHover(null)}
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
      <span className="text-xs text-faint tabular-nums">
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
    <div className="flex gap-3 border-b border-line-soft px-6 py-3">
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
        <span className="mt-1 flex items-center gap-2 text-xs text-muted">
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
    <div className={`flex gap-0.5 ${height}`} role="img" aria-label={label}>
      {parts
        .filter((p) => p.secs > 0)
        .map((p) => (
          <div
            key={p.step}
            title={p.title}
            className="min-w-0.5 first:rounded-l last:rounded-r hover:opacity-80"
            style={{ flexGrow: p.secs, background: stepInfo[p.step][1] }}
          />
        ))}
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
      <div className="mb-2 flex items-baseline justify-between">
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

/** One PR's steps, end to end. */
function StepBar({ steps }: { steps: StepTimeView[] }) {
  const label = steps
    .filter((s) => s.secs > 0)
    .map((s) => `${stepInfo[s.step][0]} ${duration(s.secs)}`)
    .join(", ");
  return (
    <div className="mt-2">
      <Bar
        height="h-1.5"
        label={`Time in each step: ${label}`}
        parts={steps.map((s) => ({
          step: s.step,
          secs: s.secs,
          title: `${stepInfo[s.step][0]}: ${duration(s.secs)}`,
        }))}
      />
    </div>
  );
}
