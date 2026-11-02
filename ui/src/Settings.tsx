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
import { ReviewSettings } from "./ReviewSettings";
import { danger, ErrorText, field, primary, Section, Status, secondary, timeAgo } from "./ui";

const link =
  "text-fg-2 underline decoration-line-strong underline-offset-2 hover:text-fg hover:decoration-fg";

/** What a key looks like, for the empty field. */
const PLACEHOLDERS: Record<string, string> = {
  anthropic: "sk-ant-…",
  openai: "sk-…",
  openrouter: "sk-or-…",
};

/** The sections, in order, for the menu beside them. */
const SECTIONS = [
  ["plan", "Plan"],
  ["models", "Model access"],
  ["review", "Automatic review"],
  ["repos", "Repo overrides"],
] as const;

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
    <div className="h-full overflow-y-auto">
      <div className="mx-auto flex max-w-6xl gap-10 px-4 py-6 sm:px-6 sm:py-8">
        <nav className="sticky top-0 h-fit w-44 shrink-0 max-xl:hidden">
          <h1 className="mb-3 text-xl font-semibold tracking-tight">Settings</h1>
          {SECTIONS.map(([id, label]) => (
            <a
              key={id}
              href={`#${id}`}
              className="block rounded-md px-2 py-1.5 text-fg-2 hover:bg-sunken hover:text-fg"
            >
              {label}
            </a>
          ))}
        </nav>
        <div className="min-w-0 flex-1 divide-y divide-line">
          <h1 className="pb-6 text-xl font-semibold tracking-tight xl:hidden">Settings</h1>
          {billing.data && <Billing workspace={workspace} billing={billing.data} />}
          <Section
            id="models"
            title="Model access"
            sub={
              <>
                Resolves rebase conflicts and fixes failed checks. Each repo's config picks the
                provider. Keys are encrypted and never shown again. For Anthropic, use a dedicated
                key in a{" "}
                <a
                  href="https://platform.claude.com/settings/workspaces"
                  target="_blank"
                  rel="noreferrer"
                  className={link}
                >
                  workspace with a spend limit
                </a>
                .
              </>
            }
          >
            <div className="divide-y divide-line-soft rounded-lg border border-line bg-surface">
              {settings.data.api_keys.flatMap((k) => [
                <KeyForm key={k.provider.id} workspace={workspace} apiKey={k} />,
                k.provider.id === "openai" && (
                  <ChatGptForm
                    key="chatgpt"
                    workspace={workspace}
                    chatgpt={settings.data.chatgpt}
                  />
                ),
              ])}
            </div>
          </Section>
          <ReviewSettings workspace={workspace} />
        </div>
      </div>
    </div>
  );
}

/** A row's buttons: one height, smaller than a form's. */
const sm = "h-8 px-3 text-[13px]";

/** The plan in one line. */
function planText(b: BillingView) {
  const price = `$${b.pricing.price_usd}/seat/month`;
  switch (b.access) {
    case "new":
      return `Private repos: free for ${b.pricing.trial_days} days, then ${price}.`;
    case "trial": {
      const left = b.trial_days_left ?? 0;
      return `Free trial: ${left} ${left === 1 ? "day" : "days"} left, then ${price}.`;
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
    <Section id="plan" title="Plan" sub={planText(billing)}>
      <div className="rounded-lg border border-line bg-surface p-4">
        <div className="flex flex-wrap items-center justify-between gap-2">
          <span className="text-xs font-medium text-muted">
            {n === 0 ? "No seats yet" : `${n} ${n === 1 ? "seat" : "seats"}`}
          </span>
          <span className="flex gap-2">
            {billing.manageable && (
              <button
                type="button"
                onClick={() => portal.mutate()}
                disabled={portal.isPending}
                className={`${secondary} ${sm}`}
              >
                Manage billing
              </button>
            )}
            {!live && (
              <button
                type="button"
                onClick={() => checkout.mutate()}
                disabled={checkout.isPending}
                className={`${primary} ${sm}`}
              >
                {billing.access === "new" ? "Start free trial" : "Subscribe"}
              </button>
            )}
          </span>
        </div>
        {n > 0 && (
          <ul className="mt-2 gap-6 sm:columns-2">
            {billing.seats.map((login) => (
              <li
                key={login}
                className="flex break-inside-avoid items-center justify-between border-b border-line-soft py-1.5"
              >
                <span className="font-mono text-[13px]">{login}</span>
                <button
                  type="button"
                  onClick={() => free.mutate(login)}
                  disabled={free.isPending}
                  className="text-xs text-muted hover:text-bad"
                >
                  Remove
                </button>
              </li>
            ))}
          </ul>
        )}
        <ErrorText error={checkout.error ?? portal.error ?? free.error} />
      </div>
    </Section>
  );
}

/** A provider's key in one row: whether it is set, a field for a new one, Remove and Save. */
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
    <form onSubmit={submit} className="p-4">
      <div className="flex flex-wrap items-center gap-x-3 gap-y-2">
        <span className="w-24 shrink-0 font-medium">{provider.name}</span>
        <Status ok={!!key} title={key?.updated_at} className="w-32 shrink-0">
          {key ? `••••${key.hint} · ${timeAgo(key.updated_at)}` : "Not set"}
        </Status>
        <input
          type="password"
          autoComplete="off"
          aria-label={`${provider.name} API key`}
          value={value}
          onChange={(e) => setValue(e.target.value)}
          placeholder={key ? "New key" : (PLACEHOLDERS[provider.id] ?? "")}
          className={`${field} h-8 min-w-40 flex-1 font-mono placeholder:font-sans`}
        />
        <span className="flex shrink-0 gap-2">
          {key && (
            <button
              type="button"
              onClick={() => remove.mutate()}
              disabled={remove.isPending}
              className={`${danger} ${sm}`}
            >
              Remove
            </button>
          )}
          <button
            type="submit"
            disabled={!value.trim() || save.isPending}
            className={`${primary} ${sm}`}
          >
            {save.isPending ? "Checking…" : "Save"}
          </button>
        </span>
      </div>
      <ErrorText error={save.error ?? remove.error} />
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
    <div className="p-4">
      <div className="flex flex-wrap items-center gap-x-3 gap-y-2">
        <span className="w-24 shrink-0 font-medium">ChatGPT plan</span>
        <Status ok={!!login} title={login?.updated_at} className="min-w-0 flex-1">
          {login ? `Signed in as ${login.hint} · ${timeAgo(login.updated_at)}` : "Not signed in"}
        </Status>
        <span className="ml-auto flex shrink-0 gap-2">
          {(login || sign_in) && (
            <button
              type="button"
              onClick={() => remove.mutate()}
              disabled={remove.isPending}
              className={`${login ? danger : secondary} ${sm}`}
            >
              {login ? "Sign out" : "Cancel"}
            </button>
          )}
          {!sign_in && (
            <button
              type="button"
              onClick={() => start.mutate()}
              disabled={start.isPending}
              className={`${login ? secondary : primary} ${sm}`}
            >
              {login ? "Sign in again" : "Sign in with ChatGPT"}
            </button>
          )}
        </span>
      </div>
      <p className="mt-1.5 text-xs text-faint sm:pl-27">
        Runs Codex on your ChatGPT plan instead of the OpenAI key. OpenAI could stop this sign-in
        any time, so keep a key to fall back on.
      </p>
      {sign_in && (
        <div className="mt-3 flex flex-col gap-1.5 rounded-md bg-sunken p-3 sm:ml-27">
          <p>
            Open{" "}
            <a href={sign_in.verification_url} target="_blank" rel="noreferrer" className={link}>
              {sign_in.verification_url}
            </a>
            , sign in, and enter:
          </p>
          <p className="font-mono text-xl tracking-widest">{sign_in.user_code}</p>
          <p className="text-xs text-muted">Waiting for the code. It works for 15 minutes.</p>
        </div>
      )}
      <ErrorText error={start.error ?? poll.error ?? remove.error} />
    </div>
  );
}
