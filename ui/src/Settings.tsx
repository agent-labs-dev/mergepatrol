import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import { type FormEvent, useState } from "react";
import { capture } from "./analytics";
import { deleteApiKey, fetchSettings, setApiKey } from "./api";
import type { ApiKeyView } from "./api/ApiKeyView";
import type { SettingsView } from "./api/SettingsView";
import { danger, ErrorText, field, Narrow, primary, timeAgo } from "./ui";

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
  if (!settings.data) return null;
  return (
    <Narrow title="Settings" sub="For this workspace only.">
      <div className="flex flex-col gap-4">
        {settings.data.api_keys.map((k) => (
          <KeyForm key={k.provider.id} workspace={workspace} apiKey={k} />
        ))}
      </div>
    </Narrow>
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
      <div className="p-5">
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
      <div className="flex items-center justify-between rounded-b-lg border-t border-line bg-page px-5 py-3">
        {key ? (
          <span className="text-muted" title={key.updated_at}>
            Ends in <span className="font-mono">{key.hint}</span>, updated {timeAgo(key.updated_at)}
          </span>
        ) : (
          <span className="text-muted">Not set.</span>
        )}
        <span className="flex gap-2">
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
