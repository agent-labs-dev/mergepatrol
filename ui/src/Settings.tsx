import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import { type FormEvent, useState } from "react";
import { capture } from "./analytics";
import { deleteAnthropicKey, fetchSettings, setAnthropicKey } from "./api";
import { danger, ErrorText, field, Narrow, primary, timeAgo } from "./ui";

export function Settings({ workspace }: { workspace: number }) {
  const queryClient = useQueryClient();
  const settings = useQuery({
    queryKey: ["settings", workspace],
    queryFn: () => fetchSettings(workspace),
  });
  const [value, setValue] = useState("");

  const onSaved = (data: Awaited<ReturnType<typeof fetchSettings>>) => {
    queryClient.setQueryData(["settings", workspace], data);
    setValue("");
  };
  const save = useMutation({
    mutationFn: (value: string) => setAnthropicKey(workspace, value),
    onSuccess: (data) => {
      capture("api key set");
      onSaved(data);
    },
  });
  const remove = useMutation({
    mutationFn: () => deleteAnthropicKey(workspace),
    onSuccess: (data) => {
      capture("api key removed");
      onSaved(data);
    },
  });

  if (!settings.data) return null;
  const { anthropic_api_key: key } = settings.data;

  const submit = (e: FormEvent) => {
    e.preventDefault();
    if (value.trim()) save.mutate(value);
  };

  return (
    <Narrow title="Settings" sub="For this workspace only.">
      <form onSubmit={submit} className="rounded-lg border border-line bg-surface">
        <div className="p-5">
          <h2 className="font-semibold">Anthropic API key</h2>
          <p className="mt-1 text-muted">
            The agent uses it when a rebase has conflicts, and to fix failed checks. It is stored
            encrypted, and never shown again.
          </p>
          <input
            type="password"
            autoComplete="off"
            value={value}
            onChange={(e) => setValue(e.target.value)}
            placeholder={key ? `Replace the key ending in ${key.hint}` : "sk-ant-…"}
            className={`${field} mt-4 w-full font-mono placeholder:font-sans`}
          />
          <ErrorText error={save.error ?? remove.error} />
        </div>
        <div className="flex items-center justify-between rounded-b-lg border-t border-line bg-page px-5 py-3">
          {key ? (
            <span className="text-muted" title={key.updated_at}>
              Ends in <span className="font-mono">{key.hint}</span>, updated{" "}
              {timeAgo(key.updated_at)}
            </span>
          ) : (
            <span className="text-warn">Not set. Conflicts cannot be resolved without it.</span>
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
    </Narrow>
  );
}
