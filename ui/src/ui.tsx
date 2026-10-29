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
      <div className="mx-auto max-w-2xl px-6 py-10">
        <h1 className="text-2xl font-semibold tracking-tight">{title}</h1>
        <p className="mt-1 mb-6 text-muted">{sub}</p>
        {children}
      </div>
    </div>
  );
}

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
