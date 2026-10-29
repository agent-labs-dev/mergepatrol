import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import { type FormEvent, useEffect, useState } from "react";
import { capture } from "./analytics";
import {
  deleteApiKey,
  deleteChatGptLogin,
  fetchBilling,
  fetchSettings,
  freeSeat,
  openPortal,
  pollChatGptLogin,
  setApiKey,
  startChatGptLogin,
  startCheckout,
} from "./api";
import type { ApiKeyView } from "./api/ApiKeyView";
import type { BillingView } from "./api/BillingView";
import type { ChatGptView } from "./api/ChatGptView";
import type { SettingsView } from "./api/SettingsView";
import { danger, ErrorText, field, Narrow, primary, secondary, timeAgo } from "./ui";

const link =
  "text-fg-2 underline decoration-line-strong underline-offset-2 hover:text-fg hover:decoration-fg";

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
          <div key={k.provider.id} className="contents">
            <KeyForm workspace={workspace} apiKey={k} />
            {k.provider.id === "openai" && (
              <ChatGptForm workspace={workspace} chatgpt={settings.data.chatgpt} />
            )}
          </div>
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
              className={link}
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

/** Signs the workspace in to a ChatGPT plan, with a code the user enters at OpenAI. */
function ChatGptForm({ workspace, chatgpt }: { workspace: number; chatgpt: ChatGptView }) {
  const queryClient = useQueryClient();
  const { login, sign_in } = chatgpt;
  const onSettings = (data: SettingsView) =>
    queryClient.setQueryData(["settings", workspace], data);

  const start = useMutation({
    mutationFn: () => startChatGptLogin(workspace),
    onSuccess: onSettings,
  });
  const poll = useMutation({
    mutationFn: () => pollChatGptLogin(workspace),
    onSuccess: (data) => {
      if (data.chatgpt.login && !login) capture("chatgpt connected");
      onSettings(data);
    },
    // The sign-in is over: the server dropped it.
    onError: () => queryClient.invalidateQueries({ queryKey: ["settings", workspace] }),
  });
  const remove = useMutation({
    mutationFn: () => deleteChatGptLogin(workspace),
    onSuccess: (data) => {
      if (login) capture("chatgpt disconnected");
      onSettings(data);
    },
  });

  // Asks until the user has entered the code, or the code expires.
  const code = sign_in?.user_code;
  const interval = (sign_in?.interval_secs ?? 5) * 1000;
  const { mutate, isPending } = poll;
  useEffect(() => {
    if (!code) return;
    const timer = setInterval(() => {
      if (!isPending) mutate();
    }, interval);
    return () => clearInterval(timer);
  }, [code, interval, mutate, isPending]);

  return (
    <div className="rounded-lg border border-line bg-surface">
      <div className="p-4 sm:p-5">
        <h2 className="font-semibold">ChatGPT plan</h2>
        <p className="mt-1 text-muted">
          Codex runs on your ChatGPT plan instead of the OpenAI API key, in repos whose config
          chooses OpenAI. Mergepatrol signs in the way the Codex CLI does, which OpenAI could stop
          at any time: keep an API key to switch back to.
        </p>
        {sign_in && (
          <div className="mt-4 flex flex-col gap-2">
            <p>
              Open{" "}
              <a href={sign_in.verification_url} target="_blank" rel="noreferrer" className={link}>
                {sign_in.verification_url}
              </a>
              , sign in, and enter this code:
            </p>
            <p className="font-mono text-xl tracking-widest">{sign_in.user_code}</p>
            <p className="text-muted">
              Waiting for you to enter it. The code works for 15 minutes.
            </p>
          </div>
        )}
        <ErrorText error={start.error ?? poll.error ?? remove.error} />
      </div>
      <div className="flex flex-wrap items-center justify-between gap-3 rounded-b-lg border-t border-line bg-page px-4 py-3 sm:px-5">
        {login ? (
          <span className="text-muted" title={login.updated_at}>
            Signed in as <span className="font-mono">{login.hint}</span>, updated{" "}
            {timeAgo(login.updated_at)}
          </span>
        ) : (
          <span className="text-muted">Not signed in.</span>
        )}
        <span className="ml-auto flex gap-2">
          {(login || sign_in) && (
            <button
              type="button"
              onClick={() => remove.mutate()}
              disabled={remove.isPending}
              className={login ? danger : secondary}
            >
              {login ? "Sign out" : "Cancel"}
            </button>
          )}
          {!sign_in && (
            <button
              type="button"
              onClick={() => start.mutate()}
              disabled={start.isPending}
              className={primary}
            >
              {login ? "Sign in again" : "Sign in with ChatGPT"}
            </button>
          )}
        </span>
      </div>
    </div>
  );
}
