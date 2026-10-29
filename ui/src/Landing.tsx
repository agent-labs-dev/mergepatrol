import type { ReactNode } from "react";
import type { PricingView } from "./api/PricingView";
import type { ThemeProps } from "./theme";
import { Mark, primary, repoUrl, StarButton, secondary, ThemeToggle } from "./ui";

const steps: [string, ReactNode][] = [
  [
    "Label it",
    <>
      Add the <Code>mergepatrol</Code> label to a pull request. <Code>mergepatrol:priority</Code>{" "}
      jumps the queue.
    </>,
  ],
  [
    "Mergepatrol tests it",
    "It rebases the PR onto its base branch, or batches it with the PRs ahead of it, and waits for the required checks.",
  ],
  [
    "It merges",
    "When the checks pass, the PR merges. When they fail, it gets ejected, and the rest merge.",
  ],
];

const features: [string, ReactNode][] = [
  [
    "Batches",
    "Queued PRs that passed their own checks are tested together, in one CI run. A failed batch splits in half until it finds the PR that broke it.",
  ],
  [
    "Agents resolve conflicts",
    "A plain git rebase runs first. Only on a real conflict does an LLM resolve it, then your verify commands run before anything is pushed.",
  ],
  [
    "Isolated jobs",
    "Each rebase runs in its own sandbox, with a token that can only read the one repo. The result must change no file the PR did not change.",
  ],
  [
    "Config in the repo",
    <>
      Each repo sets its queue in <Code>.mergepatrol/mergepatrol.yml</Code> on its default branch,
      so a PR cannot change its own settings.
    </>,
  ],
];

function Code({ children }: { children: ReactNode }) {
  return <code className="rounded bg-sunken px-1 font-mono text-[0.9em]">{children}</code>;
}

function SignInButton({ className = "" }: { className?: string }) {
  return (
    <a className={`${primary} ${className}`} href="/auth/login">
      Sign in with GitHub
    </a>
  );
}

function Plan({
  name,
  price,
  per,
  points,
}: {
  name: string;
  price: string;
  per: string;
  points: ReactNode[];
}) {
  return (
    <div className="flex flex-col rounded-xl border border-line bg-surface p-6 shadow-xs">
      <h3 className="font-medium">{name}</h3>
      <p className="mt-3">
        <span className="text-3xl font-semibold tracking-tight">{price}</span>{" "}
        <span className="text-fg-2">{per}</span>
      </p>
      <ul className="mt-5 flex flex-col gap-2 leading-relaxed text-fg-2">
        {points.map((point, i) => (
          // biome-ignore lint/suspicious/noArrayIndexKey: a fixed list
          <li key={i} className="flex gap-2">
            <span className="text-muted">–</span>
            <span>{point}</span>
          </li>
        ))}
      </ul>
    </div>
  );
}

/** The hosted plan. Self-hosted, the server sends none, and the page shows none. */
function Pricing({ pricing }: { pricing: PricingView }) {
  return (
    <section id="pricing" className="border-t border-line py-16">
      <h2 className="text-xl font-semibold tracking-tight">Pricing</h2>
      <p className="mt-2 text-fg-2">
        Every plan runs the agent on your own model API key, so LLM costs are yours, at cost.
      </p>
      <div className="mt-8 grid gap-4 sm:grid-cols-2">
        <Plan
          name="Public repos"
          price="Free"
          per="for open source"
          points={[
            "Every feature: batches, stacks, agents on conflicts and failed checks",
            `Fair use: up to ${pricing.fair_use_prs} queued PRs and ${pricing.fair_use_jobs} jobs an hour per account`,
          ]}
        />
        <Plan
          name="Private repos"
          price={`$${pricing.price_usd}`}
          per="per seat, a month"
          points={[
            `A free trial of ${pricing.trial_days} days, no card needed`,
            "A seat is a developer who queues PRs, taken with their first one",
            "Free a seat in Settings any time",
          ]}
        />
      </div>
      <p className="mt-6 text-sm text-muted">
        Or self-host it: Mergepatrol is Apache-2.0, with no limits.
      </p>
    </section>
  );
}

/** What a signed-out visitor sees: what Mergepatrol does, and the way in. */
export function Landing({
  theme,
  setTheme,
  pricing,
}: ThemeProps & { pricing: PricingView | null }) {
  return (
    <div className="min-h-screen">
      <header className="mx-auto flex h-14 max-w-5xl items-center gap-2 px-6 font-semibold">
        <Mark /> Mergepatrol
        <div className="ml-auto flex items-center gap-2 font-normal sm:gap-3">
          <ThemeToggle theme={theme} onChange={setTheme} />
          {pricing && (
            <a className="hidden text-fg-2 hover:text-fg sm:inline" href="#pricing">
              Pricing
            </a>
          )}
          <StarButton />
          <a className={`${secondary} whitespace-nowrap`} href="/auth/login">
            Sign in
          </a>
        </div>
      </header>

      <main className="mx-auto max-w-5xl px-6">
        <section className="py-20 text-center sm:py-28">
          <img src="/logo.svg" alt="" className="mx-auto size-16 rounded-2xl shadow-sm" />
          <h1 className="mx-auto mt-8 max-w-2xl text-4xl font-semibold tracking-tight text-balance sm:text-5xl">
            A merge queue for GitHub, with an agent on conflicts
          </h1>
          <p className="mx-auto mt-5 max-w-xl text-base leading-relaxed text-fg-2 text-balance">
            Label a pull request. Mergepatrol rebases it, tests it with the PRs around it, and
            merges it. When a rebase conflicts, an LLM resolves it.
          </p>
          <SignInButton className="mt-8 h-10 px-5" />
        </section>

        <section className="border-t border-line py-16">
          <h2 className="text-xl font-semibold tracking-tight">How it works</h2>
          <ol className="mt-8 grid gap-6 sm:grid-cols-3">
            {steps.map(([title, body], i) => (
              <li key={title}>
                <span className="grid size-7 place-items-center rounded-full bg-fg text-xs font-semibold text-surface">
                  {i + 1}
                </span>
                <h3 className="mt-3 font-medium">{title}</h3>
                <p className="mt-1 leading-relaxed text-fg-2">{body}</p>
              </li>
            ))}
          </ol>
        </section>

        <section className="border-t border-line py-16">
          <h2 className="text-xl font-semibold tracking-tight">Built for busy repos</h2>
          <div className="mt-8 grid gap-4 sm:grid-cols-2">
            {features.map(([title, body]) => (
              <div key={title} className="rounded-xl border border-line bg-surface p-6 shadow-xs">
                <h3 className="font-medium">{title}</h3>
                <p className="mt-1.5 leading-relaxed text-fg-2">{body}</p>
              </div>
            ))}
          </div>
        </section>

        {pricing && <Pricing pricing={pricing} />}

        <section className="border-t border-line py-16 text-center">
          <h2 className="text-xl font-semibold tracking-tight">Start queueing</h2>
          <p className="mt-2 text-fg-2">
            Sign in, install the App on your repos, and bring your own Anthropic or OpenAI API key.
          </p>
          <SignInButton className="mt-6" />
        </section>
      </main>

      <footer className="border-t border-line py-6 text-center text-xs text-muted">
        Mergepatrol · Apache-2.0 ·{" "}
        <a className="hover:text-fg" href={repoUrl}>
          GitHub
        </a>
      </footer>
    </div>
  );
}
