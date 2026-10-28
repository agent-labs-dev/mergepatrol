import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import { type FormEvent, useState } from "react";
import { deleteAnthropicKey, fetchSettings, setAnthropicKey } from "./api";

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
    onSuccess: onSaved,
  });
  const remove = useMutation({
    mutationFn: () => deleteAnthropicKey(workspace),
    onSuccess: onSaved,
  });

  if (!settings.data) return null;
  const { anthropic_api_key: key } = settings.data;

  const submit = (e: FormEvent) => {
    e.preventDefault();
    if (value.trim()) save.mutate(value);
  };

  return (
    <section className="mb-8 rounded border border-neutral-800 p-4">
      <h2 className="mb-1 font-semibold">Anthropic API key</h2>
      <p className="mb-3 text-sm text-neutral-400">
        This workspace's own key. The resolver uses it when a rebase has conflicts. It is stored
        encrypted, and never shown again.
      </p>

      <p className="mb-3 text-sm">
        {key ? (
          <>
            Set: <span className="font-mono">…{key.hint}</span>{" "}
            <span className="text-neutral-500">(updated {key.updated_at})</span>
          </>
        ) : (
          <span className="text-amber-300">Not set. Conflicts cannot be resolved without it.</span>
        )}
      </p>
      <form onSubmit={submit} className="flex gap-2">
        <input
          type="password"
          autoComplete="off"
          placeholder={key ? "Replace the key" : "sk-ant-…"}
          value={value}
          onChange={(e) => setValue(e.target.value)}
          className="flex-1 rounded border border-neutral-700 bg-neutral-900 px-3 py-1.5 font-mono text-sm"
        />
        <button
          type="submit"
          disabled={!value.trim() || save.isPending}
          className="rounded bg-sky-700 px-3 py-1.5 text-sm hover:bg-sky-600 disabled:opacity-50"
        >
          {save.isPending ? "Checking…" : "Save"}
        </button>
        {key && (
          <button
            type="button"
            onClick={() => remove.mutate()}
            disabled={remove.isPending}
            className="rounded border border-neutral-700 px-3 py-1.5 text-sm hover:bg-neutral-800"
          >
            Remove
          </button>
        )}
      </form>
      {(save.error || remove.error) && (
        <p className="mt-2 text-sm text-red-400">{(save.error ?? remove.error)?.message}</p>
      )}
    </section>
  );
}
