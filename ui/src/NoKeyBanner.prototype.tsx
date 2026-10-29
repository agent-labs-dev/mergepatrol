// PROTOTYPE, delete me: three ways to say the workspace has no Anthropic API key, on the real
// dashboard, switchable via `?variant=A|B|C`.
import { useMutation, useQueryClient } from "@tanstack/react-query";
import { Link } from "@tanstack/react-router";
import { type FormEvent, useState } from "react";
import { setAnthropicKey } from "./api";
import { ErrorText, field, primary, secondary } from "./ui";

export const variants = {
  A: "Strip above the header",
  B: "Inline setup card",
  C: "Gate, then header pill",
};

const consequence =
  "Without it, Mergepatrol can't resolve rebase conflicts or fix failed checks, so those PRs leave the queue.";

function WarnIcon({ className = "size-4" }: { className?: string }) {
  return (
    <svg viewBox="0 0 16 16" className={`${className} shrink-0 fill-current`} aria-hidden>
      <path d="M8.9 1.5a1 1 0 0 0-1.8 0L.6 13a1 1 0 0 0 .9 1.5h13a1 1 0 0 0 .9-1.5L8.9 1.5ZM7.25 6h1.5v4h-1.5V6Zm0 5.25h1.5v1.5h-1.5v-1.5Z" />
    </svg>
  );
}

/** The key form, saving straight from the banner. */
function KeyForm({ workspace, compact }: { workspace: number; compact?: boolean }) {
  const queryClient = useQueryClient();
  const [value, setValue] = useState("");
  const save = useMutation({
    mutationFn: (v: string) => setAnthropicKey(workspace, v),
    onSuccess: (data) => queryClient.setQueryData(["settings", workspace], data),
  });
  const submit = (e: FormEvent) => {
    e.preventDefault();
    if (value.trim()) save.mutate(value);
  };
  return (
    <form onSubmit={submit}>
      <div className="flex gap-2">
        <input
          type="password"
          autoComplete="off"
          value={value}
          onChange={(e) => setValue(e.target.value)}
          placeholder="sk-ant-…"
          className={`${field} min-w-0 flex-1 font-mono placeholder:font-sans ${compact ? "max-w-72" : ""}`}
        />
        <button type="submit" disabled={!value.trim() || save.isPending} className={primary}>
          {save.isPending ? "Checking…" : "Save key"}
        </button>
      </div>
      <ErrorText error={save.error} />
    </form>
  );
}

const anthropicConsole = (
  <a
    href="https://console.anthropic.com/settings/keys"
    target="_blank"
    rel="noreferrer"
    className="underline underline-offset-2 hover:opacity-70"
  >
    Anthropic Console
  </a>
);

/** A: one loud line across the top of every page, linking to Settings. */
export function Strip({ account }: { account: string }) {
  return (
    <div className="flex items-center gap-3 bg-amber-400 px-5 py-2 text-sm text-amber-950">
      <WarnIcon />
      <span>
        <span className="font-semibold">No Anthropic API key.</span> {consequence}
      </span>
      <Link
        to="/$account/settings"
        params={{ account }}
        className="ml-auto shrink-0 rounded-md bg-amber-950 px-3 py-1 font-medium text-amber-50 hover:bg-amber-900"
      >
        Add a key →
      </Link>
    </div>
  );
}

/** B: a setup card on top of each page, with the form right in it. */
export function InlineSetup({ workspace }: { workspace: number }) {
  return (
    <div className="border-b border-amber-300 bg-amber-50 px-5 py-4 dark:border-amber-400/30 dark:bg-amber-400/10">
      <div className="flex items-start gap-4">
        <div className="grid size-9 shrink-0 place-items-center rounded-full bg-amber-400/30 text-amber-700 dark:text-amber-300">
          <WarnIcon />
        </div>
        <div className="min-w-0 flex-1">
          <p className="font-semibold">Add an Anthropic API key to finish setup</p>
          <p className="mt-0.5 text-sm text-muted">
            {consequence} Create one in the {anthropicConsole}. It is stored encrypted.
          </p>
          <div className="mt-3">
            <KeyForm workspace={workspace} compact />
          </div>
        </div>
      </div>
    </div>
  );
}

/** C: blocks the page until a key is added or the user chooses to go on without one. */
export function Gate({ workspace, onSkip }: { workspace: number; onSkip: () => void }) {
  return (
    <div className="absolute inset-0 z-10 grid place-items-center bg-page/80 p-6 backdrop-blur-sm">
      <div className="w-full max-w-md rounded-xl border border-line bg-surface p-6 shadow-xl">
        <div className="flex items-center gap-2 text-amber-600 dark:text-amber-400">
          <WarnIcon className="size-5" />
          <span className="text-sm font-medium">One step left</span>
        </div>
        <h2 className="mt-3 text-xl font-semibold tracking-tight">Add an Anthropic API key</h2>
        <p className="mt-2 text-muted">{consequence}</p>
        <ul className="mt-4 space-y-1.5 text-sm">
          <li className="flex gap-2">
            <span className="text-ok">✓</span> Clean rebases and merges work now
          </li>
          <li className="flex gap-2">
            <span className="text-bad">✕</span> Resolving conflicts needs the key
          </li>
          <li className="flex gap-2">
            <span className="text-bad">✕</span> Fixing failed checks needs the key
          </li>
        </ul>
        <div className="mt-5">
          <KeyForm workspace={workspace} />
        </div>
        <p className="mt-3 text-sm text-muted">Create one in the {anthropicConsole}.</p>
        <button
          type="button"
          onClick={onSkip}
          className={`${secondary} mt-5 w-full border-transparent shadow-none`}
        >
          Continue without a key
        </button>
      </div>
    </div>
  );
}

/** C, after skipping: a pill in the header that stays until there is a key. */
export function HeaderPill({ account }: { account: string }) {
  return (
    <Link
      to="/$account/settings"
      params={{ account }}
      className="flex items-center gap-1.5 rounded-full bg-amber-400/20 px-2.5 py-0.5 text-xs font-medium text-amber-800 ring-1 ring-amber-500/40 hover:bg-amber-400/30 dark:text-amber-300"
    >
      <WarnIcon className="size-3" />
      No API key
    </Link>
  );
}
