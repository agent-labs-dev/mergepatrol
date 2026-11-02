import { useQuery } from "@tanstack/react-query";
import type { ReactNode } from "react";
import { type Theme, themes } from "./theme";

export const primary =
  "inline-flex h-9 items-center justify-center rounded-md bg-fg px-4 text-sm font-medium text-surface hover:bg-fg-2 disabled:opacity-40";
const outlined =
  "inline-flex h-9 items-center justify-center rounded-md border border-line-strong bg-surface px-4 text-sm shadow-xs hover:bg-page disabled:opacity-40";
export const secondary = `${outlined} text-fg-2`;
export const danger = `${outlined} text-bad`;
export const field =
  "h-9 rounded-md border border-line-strong bg-surface px-3 text-sm shadow-xs outline-none placeholder:text-faint focus:border-fg focus:ring-2 focus:ring-fg/10";

const repo = "agent-labs-dev/mergepatrol";
export const repoUrl = `https://github.com/${repo}`;
export const feedbackUrl = `${repoUrl}/issues/new?template=feedback.yml`;
/** The menu bar app, rebuilt from main by tray.yml. */
export const trayDownloadUrl = `${repoUrl}/releases/download/tray-latest/Mergepatrol.zip`;

export function GitHubIcon({ className = "size-4" }: { className?: string }) {
  return (
    <svg viewBox="0 0 16 16" className={`${className} fill-current`} aria-hidden>
      <path d="M8 0C3.58 0 0 3.58 0 8c0 3.54 2.29 6.53 5.47 7.59.4.07.55-.17.55-.38 0-.19-.01-.82-.01-1.49-2.01.37-2.53-.49-2.69-.94-.09-.23-.48-.94-.82-1.13-.28-.15-.68-.52-.01-.53.63-.01 1.08.58 1.23.82.72 1.21 1.87.87 2.33.66.07-.52.28-.87.51-1.07-1.78-.2-3.64-.89-3.64-3.95 0-.87.31-1.59.82-2.15-.08-.2-.36-1.02.08-2.12 0 0 .67-.21 2.2.82.64-.18 1.32-.27 2-.27.68 0 1.36.09 2 .27 1.53-1.04 2.2-.82 2.2-.82.44 1.1.16 1.92.08 2.12.51.56.82 1.27.82 2.15 0 3.07-1.87 3.75-3.65 3.95.29.25.54.73.54 1.48 0 1.07-.01 1.93-.01 2.2 0 .21.15.46.55.38A8.013 8.013 0 0 0 16 8c0-4.42-3.58-8-8-8Z" />
    </svg>
  );
}

/** A link to the repo, with its star count once GitHub's public API returns one. */
export function StarButton() {
  const stars = useQuery({
    queryKey: ["stars"],
    queryFn: async () => {
      const res = await fetch(`https://api.github.com/repos/${repo}`);
      if (!res.ok) throw new Error(res.statusText);
      return ((await res.json()) as { stargazers_count: number }).stargazers_count;
    },
    staleTime: Number.POSITIVE_INFINITY,
    retry: false,
  });
  return (
    <a
      className={`${secondary} gap-2 px-2.5 sm:px-3`}
      href={repoUrl}
      target="_blank"
      rel="noreferrer"
      aria-label="Star MergeLoop on GitHub"
    >
      <GitHubIcon />
      <span className="max-sm:hidden">Star</span>
      {stars.data !== undefined && (
        <span className="border-l border-line pl-2 tabular-nums text-muted max-sm:hidden">
          {stars.data.toLocaleString()}
        </span>
      )}
    </a>
  );
}

export function Mark() {
  return <img src="/logo.svg" className="size-5" alt="" />;
}

export function Avatar({ src, alt, size = "size-5" }: { src: string; alt: string; size?: string }) {
  return (
    <img
      src={src}
      alt={alt}
      title={alt}
      className={`${size} shrink-0 rounded-full bg-sunken ring-1 ring-line`}
    />
  );
}

/** A repo's stand-in for an avatar: its initials on a tile, tinted by its name so each repo
 * keeps its color. "merge-patrol" and "mergePatrol" are "MP", "api" is "AP". */
export function RepoIcon({ repo, size = "size-5" }: { repo: string; size?: string }) {
  const name = repo.split("/").at(-1) ?? repo;
  const words = name
    .replace(/([a-z])([A-Z])/g, "$1 $2")
    .split(/[^a-zA-Z0-9]+/)
    .filter(Boolean);
  const initials = (words.length > 1 ? words[0][0] + words[1][0] : name.slice(0, 2)).toUpperCase();
  let hash = 0;
  for (const c of repo) hash = (hash * 31 + c.charCodeAt(0)) | 0;
  const hue = Math.abs(hash) % 360;
  return (
    <span
      aria-hidden
      className={`${size} grid shrink-0 place-items-center rounded font-mono text-[8px] leading-none font-bold`}
      style={{
        color: `oklch(0.6 0.15 ${hue})`,
        background: `oklch(0.6 0.15 ${hue} / 0.15)`,
      }}
    >
      {initials}
    </span>
  );
}

export function ErrorText({ error }: { error: Error | null }) {
  return error ? <p className="mt-2 text-sm text-bad">{error.message}</p> : null;
}

/** A single centred column: the layout of every page but the queue. */
export function Narrow({
  title,
  sub,
  children,
}: {
  title: string;
  sub: ReactNode;
  children: ReactNode;
}) {
  return (
    <div className="h-full overflow-y-auto">
      <div className="mx-auto max-w-2xl px-4 py-6 sm:px-6 sm:py-10">
        <h1 className="text-2xl font-semibold tracking-tight">{title}</h1>
        <p className="mt-1 mb-6 text-muted">{sub}</p>
        {children}
      </div>
    </div>
  );
}

/** "42s", "12m", "3.4h", "2.1d". */
export function duration(secs: number) {
  if (secs < 60) return `${Math.round(secs)}s`;
  if (secs < 3600) return `${Math.round(secs / 60)}m`;
  if (secs < 86400) return `${(secs / 3600).toFixed(1)}h`;
  return `${(secs / 86400).toFixed(1)}d`;
}

/** "$0.46". */
export const usd = (n: number) => `$${n.toFixed(2)}`;

/** "claude-opus-5-5 · high", or "Unknown model" for runs from before events named it. */
export const llmName = ({ model, effort }: { model: string | null; effort: string | null }) =>
  model ? `${model}${effort ? ` · ${effort}` : ""}` : "Unknown model";

export function timeAgo(iso: string) {
  const s = Math.round((Date.now() - new Date(iso).getTime()) / 1000);
  if (s < 60) return "just now";
  if (s < 3600) return `${Math.round(s / 60)}m ago`;
  if (s < 86400) return `${Math.round(s / 3600)}h ago`;
  return `${Math.round(s / 86400)}d ago`;
}

const themeIcons: Record<Theme, ReactNode> = {
  light: (
    <>
      <circle cx="8" cy="8" r="3" />
      <path d="M8 1.5v1.5M8 13v1.5M1.5 8H3M13 8h1.5M3.4 3.4l1 1M11.6 11.6l1 1M3.4 12.6l1-1M11.6 4.4l1-1" />
    </>
  ),
  dark: <path d="M13.5 9.5A5.5 5.5 0 0 1 6.5 2.5a5.5 5.5 0 1 0 7 7Z" />,
  system: (
    <>
      <rect x="1.5" y="2.5" width="13" height="9" rx="1.5" />
      <path d="M5.5 14h5M8 11.5V14" />
    </>
  ),
};

export function ThemeToggle({ theme, onChange }: { theme: Theme; onChange: (t: Theme) => void }) {
  return (
    <fieldset className="flex rounded-md bg-sunken p-0.5">
      <legend className="sr-only">Theme</legend>
      {themes.map((t) => (
        <label
          key={t}
          title={t[0].toUpperCase() + t.slice(1)}
          className="grid size-6 cursor-pointer place-items-center rounded text-muted hover:text-fg has-checked:bg-surface has-checked:text-fg has-checked:shadow-xs has-checked:ring-1 has-checked:ring-line has-focus-visible:ring-2 has-focus-visible:ring-fg/30"
        >
          <input
            type="radio"
            name="theme"
            value={t}
            checked={t === theme}
            onChange={() => onChange(t)}
            className="sr-only"
            aria-label={t}
          />
          <svg
            viewBox="0 0 16 16"
            className="size-3.5 fill-none stroke-current"
            strokeWidth="1.5"
            strokeLinecap="round"
            aria-hidden
          >
            {themeIcons[t]}
          </svg>
        </label>
      ))}
    </fieldset>
  );
}
