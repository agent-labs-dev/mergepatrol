import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import { type FormEvent, useState } from "react";
import { capture } from "./analytics";
import {
  deleteApiKey,
  fetchBilling,
  fetchSettings,
  freeSeat,
  openPortal,
  setApiKey,
  startCheckout,
} from "./api";
import type { ApiKeyView } from "./api/ApiKeyView";
import type { BillingView } from "./api/BillingView";
import type { SettingsView } from "./api/SettingsView";
import { danger, ErrorText, field, Narrow, primary, secondary, timeAgo } from "./ui";

/** What a key looks like, for the empty field. */
const PLACEHOLDERS: Record<string, string> = {
  anthropic: "sk-ant-…",
  openai: "sk-…",
  openrouter: "sk-or-…",
};

export function Settings({ workspace }: { workspace: number }) {
  const settings = useQuery({
    queryKey: ["settings", workspace],
    queryFn: () => fetchSettings(workspace),
  });
  const billing = useQuery({
    queryKey: ["billing", workspace],
    queryFn: () => fetchBilling(workspace),
  });
  if (!settings.data) return null;
  return (
    <Narrow title="Settings" sub="For this workspace only.">
      <div className="flex flex-col gap-4">
        {billing.data && <Billing workspace={workspace} billing={billing.data} />}
        {settings.data.api_keys.map((k) => (
          <KeyForm key={k.provider.id} workspace={workspace} apiKey={k} />
        ))}
      </div>
    </Narrow>
  );
}

/** The plan in one line. */
function planText(b: BillingView) {
  const price = `$${b.pricing.price_usd}/seat/month`;
  switch (b.access) {
    case "new":
      return `Private repos: free for ${b.pricing.trial_days} days, then ${price}.`;
    case "trial": {
      const left = b.trial_days_left ?? 0;
      return `Free trial: ${left} ${left === 1 ? "day" : "days"} left.`;
    }
    case "subscribed":
      return `${price}.`;
    case "ended":
      return "Subscription ended. Private repos are paused.";
  }
}

function Billing({ workspace, billing }: { workspace: number; billing: BillingView }) {
  const queryClient = useQueryClient();
  const go = (url: string) => window.location.assign(url);
  const checkout = useMutation({
    mutationFn: () => startCheckout(workspace),
    onSuccess: ({ url }) => {
      capture("checkout started", { trial: billing.access === "new" });
      go(url);
    },
  });
  const portal = useMutation({
    mutationFn: () => openPortal(workspace),
    onSuccess: ({ url }) => go(url),
  });
  const free = useMutation({
    mutationFn: (login: string) => freeSeat(workspace, login),
    onSuccess: (data) => {
      capture("seat freed");
      queryClient.setQueryData(["billing", workspace], data);
    },
  });
  const live = billing.access === "trial" || billing.access === "subscribed";
  const n = billing.seats.length;

  return (
    <section className="rounded-lg border border-line bg-surface">
      <div className="p-4 sm:p-5">
        <h2 className="font-semibold">Plan</h2>
        <p className="mt-1 text-muted">{planText(billing)}</p>
        {n > 0 && (
          <div className="mt-3">
            <p>
              {n} {n === 1 ? "seat" : "seats"}
            </p>
            <ul className="mt-2 flex flex-col divide-y divide-line rounded-md border border-line">
              {billing.seats.map((login) => (
                <li key={login} className="flex items-center justify-between px-3 py-1.5">
                  <span className="font-mono">{login}</span>
                  <button
                    type="button"
                    onClick={() => free.mutate(login)}
                    disabled={free.isPending}
                    className="text-muted hover:text-fg"
                  >
                    Remove
                  </button>
                </li>
              ))}
            </ul>
          </div>
        )}
        <ErrorText error={checkout.error ?? portal.error ?? free.error} />
      </div>
      <div className="flex flex-wrap items-center justify-end gap-2 rounded-b-lg border-t border-line bg-page px-4 py-3 sm:px-5">
        {billing.manageable && (
          <button
            type="button"
            onClick={() => portal.mutate()}
            disabled={portal.isPending}
            className={secondary}
          >
            Manage billing
          </button>
        )}
        {!live && (
          <button
            type="button"
            onClick={() => checkout.mutate()}
            disabled={checkout.isPending}
            className={primary}
          >
            {billing.access === "new" ? "Start free trial" : "Subscribe"}
          </button>
        )}
      </div>
    </section>
  );
}

function KeyForm({ workspace, apiKey }: { workspace: number; apiKey: ApiKeyView }) {
  const queryClient = useQueryClient();
  const [value, setValue] = useState("");
  const { provider, key } = apiKey;

  const onSaved = (data: SettingsView) => {
    queryClient.setQueryData(["settings", workspace], data);
    setValue("");
  };
  const save = useMutation({
    mutationFn: (value: string) => setApiKey(workspace, provider.id, value),
    onSuccess: (data) => {
      capture("api key set", { provider: provider.id });
      onSaved(data);
    },
  });
  const remove = useMutation({
    mutationFn: () => deleteApiKey(workspace, provider.id),
    onSuccess: (data) => {
      capture("api key removed", { provider: provider.id });
      onSaved(data);
    },
  });

  const submit = (e: FormEvent) => {
    e.preventDefault();
    if (value.trim()) save.mutate(value);
  };

  return (
    <form onSubmit={submit} className="rounded-lg border border-line bg-surface">
      <div className="p-4 sm:p-5">
        <h2 className="font-semibold">{provider.name} API key</h2>
        <p className="mt-1 text-muted">
          The agent uses it when a rebase has conflicts, and to fix failed checks, in repos whose
          config chooses {provider.name}. It is stored encrypted, and never shown again.
        </p>
        {provider.id === "anthropic" && (
          <p className="mt-1 text-muted">
            Create a key for Mergepatrol alone, in an{" "}
            <a
              href="https://platform.claude.com/settings/workspaces"
              target="_blank"
              rel="noreferrer"
              className="text-fg-2 underline decoration-line-strong underline-offset-2 hover:text-fg hover:decoration-fg"
            >
              Anthropic workspace
            </a>{" "}
            with a spend limit. If it leaks, the limit caps what it can cost.
          </p>
        )}
        <input
          type="password"
          autoComplete="off"
          value={value}
          onChange={(e) => setValue(e.target.value)}
          placeholder={
            key ? `Replace the key ending in ${key.hint}` : (PLACEHOLDERS[provider.id] ?? "")
          }
          className={`${field} mt-4 w-full font-mono placeholder:font-sans`}
        />
        <ErrorText error={save.error ?? remove.error} />
      </div>
      <div className="flex flex-wrap items-center justify-between gap-3 rounded-b-lg border-t border-line bg-page px-4 py-3 sm:px-5">
        {key ? (
          <span className="text-muted" title={key.updated_at}>
            Ends in <span className="font-mono">{key.hint}</span>, updated {timeAgo(key.updated_at)}
          </span>
        ) : (
          <span className="text-muted">Not set.</span>
        )}
        <span className="ml-auto flex gap-2">
          {key && (
            <button
              type="button"
              onClick={() => remove.mutate()}
              disabled={remove.isPending}
              className={danger}
            >
              Remove
            </button>
          )}
          <button type="submit" disabled={!value.trim() || save.isPending} className={primary}>
            {save.isPending ? "Checking…" : "Save"}
          </button>
        </span>
      </div>
    </form>
  );
}
