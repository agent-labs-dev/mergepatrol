import { keepPreviousData, useQuery } from "@tanstack/react-query";
import { fetchReviewStats } from "./api";
import type { ChangeOutcomesView } from "./api/ChangeOutcomesView";
import { duration } from "./ui";

/** What became of each change the review asked for, as the Claude Code team sorts its review's
 * comments: fixed, wrong, intended, or dismissed. */
const outcomes: [keyof ChangeOutcomesView, string, string][] = [
  ["fixed", "Fixed", "var(--ok)"],
  ["wrong", "Wrong", "var(--bad)"],
  ["intended", "Intended", "var(--warn)"],
  ["dismissed", "Dismissed", "var(--muted)"],
  ["open", "Open", "var(--line-strong)"],
];

const pct = (n: number, of: number) => (of ? `${Math.round((n / of) * 100)}%` : "–");

/** The agent's review in a repo, or all of them: its noise, how many reviews a PR takes to
 * pass, and what it asks of people. Reviews are not scoped by author. */
export function ReviewStats({
  workspace,
  repo,
  days,
}: {
  workspace: number;
  repo?: string;
  days: number;
}) {
  const stats = useQuery({
    queryKey: ["review-stats", workspace, repo, days],
    queryFn: () => fetchReviewStats(workspace, { repo, days }),
    placeholderData: keepPreviousData,
    refetchInterval: 30_000,
  });
  if (!stats.data || stats.data.prs === 0) return null;
  const { changes, rounds, humans, prs } = stats.data;
  const asked = outcomes.reduce((sum, [k]) => sum + changes[k], 0);
  return (
    <div className="mt-5">
      <p className="mb-2 text-xs font-medium text-muted">The agent's review</p>
      {asked > 0 && (
        <>
          <span
            className="flex h-2 gap-0.5"
            role="img"
            aria-label={outcomes.map(([k, name]) => `${name} ${changes[k]}`).join(", ")}
          >
            {outcomes
              .filter(([k]) => changes[k] > 0)
              .map(([k, name, color]) => (
                <span
                  key={k}
                  title={`${name}: ${changes[k]}`}
                  className="min-w-0.5 first:rounded-l last:rounded-r"
                  style={{ flexGrow: changes[k], background: color }}
                />
              ))}
          </span>
          <p className="mt-2 flex flex-wrap gap-x-4 gap-y-1 text-xs tabular-nums">
            <span className="text-faint">{asked} changes asked for</span>
            {outcomes.map(([k, name, color]) => (
              <span key={k} className="flex items-center gap-1.5">
                <span className="size-2 rounded-sm" style={{ background: color }} />
                {name} {pct(changes[k], asked)}
              </span>
            ))}
          </p>
        </>
      )}
      <dl className="mt-3 grid grid-cols-2 gap-x-4 gap-y-2 text-xs tabular-nums sm:grid-cols-4">
        <Stat label="Pass on the first review" value={pct(rounds.first_review, rounds.passed)} />
        <Stat label="Within one fix round" value={pct(rounds.one_round, rounds.passed)} />
        <Stat label="Five reviews or more" value={pct(rounds.five_plus, rounds.passed)} />
        <Stat
          label="Reviews to pass"
          value={rounds.mean_reviews === null ? "–" : rounds.mean_reviews.toFixed(2)}
        />
        <Stat label="PRs that asked a person" value={`${humans.asked_prs} of ${prs}`} />
        <Stat
          label="Wait on a person, median"
          value={humans.wait_p50_secs === null ? "–" : duration(humans.wait_p50_secs)}
        />
      </dl>
    </div>
  );
}

function Stat({ label, value }: { label: string; value: string }) {
  return (
    <div>
      <dt className="text-faint">{label}</dt>
      <dd className="text-sm">{value}</dd>
    </div>
  );
}
