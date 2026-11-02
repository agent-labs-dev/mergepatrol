import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import { type FormEvent, type ReactNode, useId, useState } from "react";
import { capture } from "./analytics";
import { fetchRepoConfig, proposeRepoConfig } from "./api";
import type { ProviderView } from "./api/ProviderView";
import type { RepoConfigView } from "./api/RepoConfigView";
import type { RepoSettingsView } from "./api/RepoSettingsView";
import type { TaskModelsView } from "./api/TaskModelsView";
import type { TaskModelView } from "./api/TaskModelView";
import { ErrorText, field, primary } from "./ui";

/** A repo's config file, as a form. Saving opens a PR that changes the file. */
export function RepoSettings({ workspace, repo }: { workspace: number; repo: string }) {
  const config = useQuery({
    queryKey: ["repo-config", workspace, repo],
    queryFn: () => fetchRepoConfig(workspace, repo),
  });
  return (
    <div className="border-t border-line-soft bg-page px-4 py-4">
      {config.isPending && <p className="text-muted">Loading…</p>}
      {config.error && <p className="text-bad">{config.error.message}</p>}
      {config.data && <Form workspace={workspace} repo={repo} config={config.data} />}
    </div>
  );
}

function Form({
  workspace,
  repo,
  config,
}: {
  workspace: number;
  repo: string;
  config: RepoConfigView;
}) {
  const queryClient = useQueryClient();
  const id = useId();
  const [draft, setDraft] = useState<RepoSettingsView>(config.settings);
  const [verify, setVerify] = useState(config.settings.verify.join("\n"));
  const [branches, setBranches] = useState(config.settings.branches.join("\n"));
  const set = (patch: Partial<RepoSettingsView>) => setDraft({ ...draft, ...patch });
  const setTask = (task: keyof TaskModelsView, model: TaskModelView | null) =>
    set({ tasks: { ...draft.tasks, [task]: model } });
  const propose = useMutation({
    mutationFn: (settings: RepoSettingsView) => proposeRepoConfig(workspace, repo, settings),
    onSuccess: () => {
      capture("repo config proposed", { repo });
      return queryClient.invalidateQueries({ queryKey: ["repo-config", workspace, repo] });
    },
  });

  const submit = (e: FormEvent) => {
    e.preventDefault();
    propose.mutate({
      ...draft,
      verify: verify.split("\n").filter((c) => c.trim()),
      branches: branches.split("\n").filter((b) => b.trim()),
    });
  };
  const pr = propose.data ?? config.pull_request;
  const branch = <code className="text-xs">{config.default_branch}</code>;

  return (
    <form onSubmit={submit}>
      <p className="text-muted">
        {config.pull_request ? (
          <>From the open PR #{config.pull_request.number}.</>
        ) : config.has_file ? (
          <>
            From <code className="text-xs">{config.path}</code> on {branch}.
          </>
        ) : (
          <>No config file on {branch}: these are the defaults.</>
        )}{" "}
        Saving opens a PR that changes the file; the queue uses the new settings once it merges.
      </p>
      {config.error && (
        <p className="mt-2 text-warn">
          The file does not parse, so the queue is paused: {config.error}. Saving these settings
          replaces it.
        </p>
      )}
      <div className="mt-4 grid gap-3 sm:grid-cols-2">
        <Field id={`${id}-label`} label="Trigger label" hint="Adds :priority and :failed">
          <input
            id={`${id}-label`}
            value={draft.label}
            onChange={(e) => set({ label: e.target.value })}
            className={`${field} w-full`}
          />
        </Field>
        <Field id={`${id}-batch`} label="Batch size" hint="1 merges one PR at a time">
          <input
            id={`${id}-batch`}
            type="number"
            min={1}
            value={draft.batch_size}
            onChange={(e) => set({ batch_size: Number(e.target.value) })}
            className={`${field} w-full`}
          />
        </Field>
        <Field id={`${id}-provider`} label="Resolver provider" hint="Its key is in Settings">
          <select
            id={`${id}-provider`}
            value={draft.provider}
            onChange={(e) => set(pick(config.providers, e.target.value))}
            className={`${field} w-full`}
          >
            {config.providers.map((p) => (
              <option key={p.id} value={p.id}>
                {p.name}
              </option>
            ))}
          </select>
        </Field>
        <Field id={`${id}-model`} label="Resolver model">
          <ModelSelect
            id={`${id}-model`}
            providers={config.providers}
            provider={draft.provider}
            value={draft.model}
            onChange={(model) => set({ model })}
          />
        </Field>
        <Field id={`${id}-effort`} label="Resolver effort">
          <select
            id={`${id}-effort`}
            value={draft.effort}
            onChange={(e) => set({ effort: e.target.value })}
            className={`${field} w-full`}
          >
            {config.efforts.map((e) => (
              <option key={e}>{e}</option>
            ))}
          </select>
        </Field>
      </div>
      <div className="mt-3 grid gap-3 sm:grid-cols-2">
        {TASKS.map(({ task, label, hint }) => {
          const model = draft.tasks[task];
          return (
            <div key={task} className="contents">
              <Field id={`${id}-${task}-provider`} label={label} hint={hint}>
                <select
                  id={`${id}-${task}-provider`}
                  value={model?.provider ?? ""}
                  onChange={(e) =>
                    setTask(task, e.target.value ? pick(config.providers, e.target.value) : null)
                  }
                  className={`${field} w-full`}
                >
                  <option value="">The resolver's</option>
                  {config.providers.map((p) => (
                    <option key={p.id} value={p.id}>
                      {p.name}
                    </option>
                  ))}
                </select>
              </Field>
              <Field id={`${id}-${task}-model`} label={`${label} model`}>
                {model ? (
                  <ModelSelect
                    id={`${id}-${task}-model`}
                    providers={config.providers}
                    provider={model.provider}
                    value={model.model}
                    onChange={(m) => setTask(task, { ...model, model: m })}
                  />
                ) : (
                  <select
                    id={`${id}-${task}-model`}
                    disabled
                    className={`${field} w-full opacity-50`}
                  >
                    <option>{nameOf(config.providers, draft.provider, draft.model)}</option>
                  </select>
                )}
              </Field>
            </div>
          );
        })}
      </div>
      <div className="mt-3 grid gap-3 sm:grid-cols-2">
        <Field id={`${id}-fix`} label="Fix failed checks" hint="The agent commits a fix">
          <label className="flex h-9 items-center gap-2 text-fg-2">
            <input
              id={`${id}-fix`}
              type="checkbox"
              checked={draft.fix_ci}
              onChange={(e) => set({ fix_ci: e.target.checked })}
            />
            On
          </label>
        </Field>
        <Field id={`${id}-attempts`} label="Fix attempts" hint="Before the PR is ejected">
          <input
            id={`${id}-attempts`}
            type="number"
            min={1}
            max={5}
            disabled={!draft.fix_ci}
            value={draft.fix_ci_attempts}
            onChange={(e) => set({ fix_ci_attempts: Number(e.target.value) })}
            className={`${field} w-full disabled:opacity-50`}
          />
        </Field>
      </div>
      <div className="mt-3">
        <Field
          id={`${id}-verify`}
          label="Verify commands"
          hint="One per line. They run after the agent resolves or fixes"
        >
          <textarea
            id={`${id}-verify`}
            value={verify}
            onChange={(e) => setVerify(e.target.value)}
            rows={3}
            placeholder="cargo check"
            className={`${field} h-auto w-full py-2 font-mono`}
          />
        </Field>
      </div>
      <div className="mt-3">
        <Field
          id={`${id}-branches`}
          label="Other branches"
          hint="One per line. Each gets a queue, as the default branch has"
        >
          <textarea
            id={`${id}-branches`}
            value={branches}
            onChange={(e) => setBranches(e.target.value)}
            rows={2}
            placeholder="release"
            className={`${field} h-auto w-full py-2 font-mono`}
          />
        </Field>
      </div>
      <ErrorText error={propose.error} />
      <div className="mt-4 flex flex-wrap items-center justify-between gap-3">
        <span className="text-muted">
          {pr && (
            <>
              {propose.data ? "Opened " : "Open: "}
              <a
                href={pr.url}
                target="_blank"
                rel="noreferrer"
                className="font-medium text-fg underline underline-offset-2"
              >
                PR #{pr.number}
              </a>
            </>
          )}
        </span>
        <button type="submit" disabled={propose.isPending} className={primary}>
          {propose.isPending
            ? "Opening PR…"
            : pr
              ? `Update PR #${pr.number}`
              : config.has_file
                ? "Open PR"
                : "Open setup PR"}
        </button>
      </div>
    </form>
  );
}

/** The tasks that can run their own model, as the file names them. */
const TASKS: { task: keyof TaskModelsView; label: string; hint: string }[] = [
  { task: "rebase", label: "Rebase", hint: "Resolves conflicts" },
  { task: "fix", label: "Fix", hint: "Fixes failed checks" },
  { task: "review", label: "Review", hint: "Reviews, and answers questions" },
];

type ProviderId = TaskModelView["provider"];

/** A provider, with the model to start from: its first. */
function pick(providers: ProviderView[], id: string): TaskModelView {
  const provider = id as ProviderId;
  return { provider, model: providers.find((p) => p.id === id)?.models[0]?.id ?? "" };
}

/** A model's name as people write it, else its id. */
function nameOf(providers: ProviderView[], provider: ProviderId, model: string) {
  const models = providers.find((p) => p.id === provider)?.models ?? [];
  return models.find((m) => m.id === model)?.name ?? model;
}

/** The provider's models. A model the file names that the list lacks stays, as itself. */
function ModelSelect({
  id,
  providers,
  provider,
  value,
  onChange,
}: {
  id: string;
  providers: ProviderView[];
  provider: ProviderId;
  value: string;
  onChange: (model: string) => void;
}) {
  const models = providers.find((p) => p.id === provider)?.models ?? [];
  const known = models.some((m) => m.id === value);
  return (
    <select
      id={id}
      value={value}
      onChange={(e) => onChange(e.target.value)}
      className={`${field} w-full`}
    >
      {!known && <option value={value}>{value || "Pick a model"}</option>}
      {models.map((m) => (
        <option key={m.id} value={m.id}>
          {m.name}
        </option>
      ))}
    </select>
  );
}

function Field({
  id,
  label,
  hint,
  children,
}: {
  id: string;
  label: string;
  hint?: string;
  children: ReactNode;
}) {
  return (
    <div>
      <label htmlFor={id} className="text-xs font-medium text-fg-2">
        {label}
      </label>
      {hint && <span className="ml-2 text-xs text-faint">{hint}</span>}
      <div className="mt-1">{children}</div>
    </div>
  );
}
