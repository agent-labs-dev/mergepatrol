import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import { type ReactNode, useEffect, useId, useState } from "react";
import { capture } from "./analytics";
import { fetchRepoDefaults, proposeRepoConfig, setRepoDefaults } from "./api";
import type { ModelOverridesView } from "./api/ModelOverridesView";
import type { ProviderView } from "./api/ProviderView";
import type { RepoConfigView } from "./api/RepoConfigView";
import type { RepoSettingsView } from "./api/RepoSettingsView";
import type { TaskModelView } from "./api/TaskModelView";
import type { UpdatePullView } from "./api/UpdatePullView";
import type { WorkspaceDefaultsView } from "./api/WorkspaceDefaultsView";
import { reviewOverrides } from "./ReviewSettings";
import { ErrorText, field, primary, Section, secondary, Toggle } from "./ui";

/*
 * What the config file sets. The workspace's defaults save at once; a repo's own settings live in
 * its file, so an edit waits in `Drafts` until it leaves as a PR. A value equal to the workspace's
 * follows it: the API leaves it out of the file.
 */

/** The tasks that can override the default model, as the file names them. */
const TASKS: { task: keyof ModelOverridesView; label: string; hint: string }[] = [
  { task: "rebase", label: "Rebase", hint: "Resolves conflicts" },
  { task: "fix", label: "Fix", hint: "Fixes failed checks" },
  { task: "review", label: "Review", hint: "Reviews, and answers questions" },
];

const isSame = (a: unknown, b: unknown) => JSON.stringify(a) === JSON.stringify(b);

/** A model's name as people write it, else its id. */
function nameOf(providers: ProviderView[], { provider, model }: TaskModelView) {
  const models = providers.find((p) => p.id === provider)?.models ?? [];
  return models.find((m) => m.id === model)?.name ?? model;
}

const modelKey = ({ provider, model }: TaskModelView) => `${provider}|${model}`;
const parseModel = (key: string): TaskModelView => {
  const [provider, model] = key.split("|");
  return { provider: provider as TaskModelView["provider"], model };
};

/** Every model of the providers, and `value`, which a file may name without the list having it. */
function ModelOptions({ providers, value }: { providers: ProviderView[]; value?: TaskModelView }) {
  const known = providers.some((p) =>
    p.models.some((m) => value && p.id === value.provider && m.id === value.model),
  );
  return (
    <>
      {value && !known && <option value={modelKey(value)}>{value.model}</option>}
      {providers.map((p) => (
        <optgroup key={p.id} label={p.name}>
          {p.models.map((m) => (
            <option key={m.id} value={`${p.id}|${m.id}`}>
              {m.name}
            </option>
          ))}
        </optgroup>
      ))}
    </>
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

/** The workspace's defaults, which every repo follows where its file says nothing. */
export function RepoDefaults({ workspace }: { workspace: number }) {
  const defaults = useQuery({
    queryKey: ["repo-defaults", workspace],
    queryFn: () => fetchRepoDefaults(workspace),
  });
  if (!defaults.data) return null;
  return (
    <Section
      id="defaults"
      title="Repo defaults"
      sub="Every repo follows these where its config file says nothing. An empty file follows all of them. Changes apply at once, with no PR."
    >
      <DefaultsForm
        key={JSON.stringify(defaults.data.defaults)}
        workspace={workspace}
        saved={defaults.data.defaults}
        providers={defaults.data.providers}
        efforts={defaults.data.efforts}
      />
    </Section>
  );
}

function DefaultsForm({
  workspace,
  saved,
  providers,
  efforts,
}: {
  workspace: number;
  saved: WorkspaceDefaultsView;
  providers: ProviderView[];
  efforts: string[];
}) {
  const client = useQueryClient();
  const id = useId();
  const [draft, setDraft] = useState(saved);
  const set = (patch: Partial<WorkspaceDefaultsView>) => setDraft({ ...draft, ...patch });
  const save = useMutation({
    mutationFn: () => setRepoDefaults(workspace, draft),
    onSuccess: (data) => {
      capture("repo defaults saved");
      client.setQueryData(["repo-defaults", workspace], data);
      return client.invalidateQueries({ queryKey: ["repo-config", workspace] });
    },
  });
  const dirty = !isSame(draft, saved);
  return (
    <div className="rounded-lg border border-line bg-surface">
      <div className="grid gap-3 p-4 sm:grid-cols-2">
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
        <Field id={`${id}-model`} label="Default model" hint="Every task runs it unless overridden">
          <select
            id={`${id}-model`}
            value={modelKey(draft)}
            onChange={(e) => set(parseModel(e.target.value))}
            className={`${field} w-full`}
          >
            <ModelOptions providers={providers} value={draft} />
          </select>
        </Field>
        <Field id={`${id}-effort`} label="Effort" hint="Every task's">
          <select
            id={`${id}-effort`}
            value={draft.effort}
            onChange={(e) => set({ effort: e.target.value })}
            className={`${field} w-full`}
          >
            {efforts.map((e) => (
              <option key={e}>{e}</option>
            ))}
          </select>
        </Field>
        {TASKS.map(({ task, label, hint }) => (
          <Field key={task} id={`${id}-${task}`} label={`${label} model`} hint={hint}>
            <select
              id={`${id}-${task}`}
              value={draft.overrides[task] ? modelKey(draft.overrides[task]) : ""}
              onChange={(e) =>
                set({
                  overrides: {
                    ...draft.overrides,
                    [task]: e.target.value ? parseModel(e.target.value) : null,
                  },
                })
              }
              className={`${field} w-full`}
            >
              <option value="">Default: {nameOf(providers, draft)}</option>
              <ModelOptions providers={providers} value={draft.overrides[task] ?? undefined} />
            </select>
          </Field>
        ))}
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
      <div className="flex items-center justify-between gap-4 border-t border-line-soft p-4">
        <span>
          <span className="font-medium">Fix failed checks</span>
          <span className="block text-xs text-muted">The agent commits a fix.</span>
        </span>
        <Toggle
          on={draft.fix_ci}
          onChange={(fix_ci) => set({ fix_ci })}
          label="Fix failed checks"
        />
      </div>
      <div className="flex items-center gap-3 border-t border-line-soft px-4 py-3">
        <span className="text-xs text-muted">
          {dirty ? "Unsaved. Applies to every repo that follows it." : "Saved."}
        </span>
        <button
          type="button"
          disabled={!dirty || save.isPending}
          onClick={() => save.mutate()}
          className={`${primary} ml-auto h-8 px-3`}
        >
          Save
        </button>
      </div>
      {save.error && (
        <div className="px-4 pb-3">
          <ErrorText error={save.error} />
        </div>
      )}
    </div>
  );
}

/** What a repo sets over the workspace, in words. Verify commands and branches have no workspace
 * value to differ from. */
export function overrides(
  s: RepoSettingsView,
  d: RepoSettingsView,
  providers: ProviderView[],
): string[] {
  const words: string[] = [];
  if (s.label !== d.label) words.push(`Label ${s.label}`);
  if (s.batch_size !== d.batch_size) words.push(`Batch ${s.batch_size}`);
  if (s.provider !== d.provider || s.model !== d.model) words.push(nameOf(providers, s));
  if (s.effort !== d.effort) words.push(`Effort ${s.effort}`);
  for (const { task, label } of TASKS) {
    const own = s.overrides[task];
    if (!isSame(own ?? null, d.overrides[task] ?? null))
      words.push(`${label} ${own ? nameOf(providers, own) : "default"}`);
  }
  if (s.fix_ci !== d.fix_ci) words.push(`Fixes ${s.fix_ci ? "on" : "off"}`);
  if (s.fix_ci_attempts !== d.fix_ci_attempts) words.push(`${s.fix_ci_attempts} fix attempts`);
  return [...words, ...reviewOverrides(s.review)];
}

/** A repo's setting with a workspace value: empty follows it, and says what it is. */
function OwnInput({
  id,
  value,
  inherited,
  type,
  max,
  disabled,
  onChange,
}: {
  id: string;
  value: string;
  inherited: string;
  type?: "number";
  max?: number;
  disabled?: boolean;
  onChange: (value: string) => void;
}) {
  return (
    <input
      id={id}
      type={type}
      min={type && 1}
      max={max}
      disabled={disabled}
      value={value === inherited ? "" : value}
      placeholder={`Workspace: ${inherited}`}
      onChange={(e) => onChange(e.target.value || inherited)}
      className={`${field} w-full disabled:opacity-50`}
    />
  );
}

/** A repo's edits, by repo, with what the edit started from. */
export type Drafts = Record<string, { from: RepoSettingsView; to: RepoSettingsView }>;

/** A repo's config file, as a form. Its edits wait in `drafts` until they leave as a PR. */
export function RepoSettings({
  config,
  settings,
  onChange,
  review,
}: {
  config: RepoConfigView;
  settings: RepoSettingsView;
  onChange: (settings: RepoSettingsView) => void;
  /** The review's overrides, which live in the file with the rest. */
  review: ReactNode;
}) {
  const id = useId();
  const { defaults: d, providers } = config;
  const s = settings;
  const set = (patch: Partial<RepoSettingsView>) => onChange({ ...s, ...patch });
  const setTask = (task: keyof ModelOverridesView, model: TaskModelView | null) =>
    set({ overrides: { ...s.overrides, [task]: model } });
  const branch = <code className="text-xs">{config.default_branch}</code>;
  const ownModel = s.provider !== d.provider || s.model !== d.model;
  return (
    <div className="border-t border-line-soft bg-page px-5 py-4">
      <p className="text-muted">
        {config.pull_request ? (
          <>From the open PR #{config.pull_request.number}.</>
        ) : config.has_file ? (
          <>
            From <code className="text-xs">{config.path}</code> on {branch}.
          </>
        ) : (
          <>No config file on {branch}: this repo would follow the workspace.</>
        )}{" "}
        Changing a setting opens a PR, queued for you, and applies once it merges.
      </p>
      {!config.follows && (
        <p className="mt-2 text-muted">
          The file spells out settings that match the workspace, so they stay put if it changes.
          Update all trims them.
        </p>
      )}
      {config.error && (
        <p className="mt-2 text-warn">
          The file does not parse, so the queue is paused: {config.error}. Saving these settings
          replaces it.
        </p>
      )}
      <h3 className="mt-5 mb-2 text-xs font-medium text-muted">Queue and models</h3>
      <div className="grid gap-3 sm:grid-cols-2">
        <Field id={`${id}-label`} label="Trigger label" hint="Adds :priority and :failed">
          <OwnInput
            id={`${id}-label`}
            value={s.label}
            inherited={d.label}
            onChange={(label) => set({ label })}
          />
        </Field>
        <Field id={`${id}-batch`} label="Batch size" hint="1 merges one PR at a time">
          <OwnInput
            id={`${id}-batch`}
            type="number"
            value={String(s.batch_size)}
            inherited={String(d.batch_size)}
            onChange={(batch) => set({ batch_size: Number(batch) })}
          />
        </Field>
        <Field id={`${id}-model`} label="Default model" hint="Every task runs it unless overridden">
          <select
            id={`${id}-model`}
            value={ownModel ? modelKey(s) : ""}
            onChange={(e) => set(parseModel(e.target.value || modelKey(d)))}
            className={`${field} w-full`}
          >
            <option value="">Workspace: {nameOf(providers, d)}</option>
            <ModelOptions providers={providers} value={ownModel ? s : undefined} />
          </select>
        </Field>
        <Field id={`${id}-effort`} label="Effort" hint="Every task's">
          <select
            id={`${id}-effort`}
            value={s.effort === d.effort ? "" : s.effort}
            onChange={(e) => set({ effort: e.target.value || d.effort })}
            className={`${field} w-full`}
          >
            <option value="">Workspace: {d.effort}</option>
            {config.efforts.map((e) => (
              <option key={e}>{e}</option>
            ))}
          </select>
        </Field>
        {TASKS.map(({ task, label, hint }) => {
          const own = s.overrides[task] ?? null;
          const theirs = d.overrides[task] ?? null;
          const follows = isSame(own, theirs);
          return (
            <Field key={task} id={`${id}-${task}`} label={`${label} model`} hint={hint}>
              <select
                id={`${id}-${task}`}
                value={follows ? "" : own ? modelKey(own) : "default"}
                onChange={(e) =>
                  setTask(
                    task,
                    e.target.value === ""
                      ? theirs
                      : e.target.value === "default"
                        ? null
                        : parseModel(e.target.value),
                  )
                }
                className={`${field} w-full`}
              >
                <option value="">
                  Workspace: {theirs ? nameOf(providers, theirs) : "default model"}
                </option>
                {theirs && <option value="default">Default: {nameOf(providers, s)}</option>}
                <ModelOptions
                  providers={providers}
                  value={follows ? undefined : (own ?? undefined)}
                />
              </select>
            </Field>
          );
        })}
        <Field id={`${id}-fix`} label="Fix failed checks" hint="The agent commits a fix">
          <select
            id={`${id}-fix`}
            value={s.fix_ci === d.fix_ci ? "" : s.fix_ci ? "on" : "off"}
            onChange={(e) => set({ fix_ci: e.target.value ? e.target.value === "on" : d.fix_ci })}
            className={`${field} w-full`}
          >
            <option value="">Workspace: {d.fix_ci ? "On" : "Off"}</option>
            <option value="on">On</option>
            <option value="off">Off</option>
          </select>
        </Field>
        <Field id={`${id}-attempts`} label="Fix attempts" hint="Before the PR is ejected">
          <OwnInput
            id={`${id}-attempts`}
            type="number"
            max={5}
            disabled={!s.fix_ci}
            value={String(s.fix_ci_attempts)}
            inherited={String(d.fix_ci_attempts)}
            onChange={(attempts) => set({ fix_ci_attempts: Number(attempts) })}
          />
        </Field>
      </div>
      <h3 className="mt-5 mb-2 text-xs font-medium text-muted">Automatic review</h3>
      {review}
      <h3 className="mt-5 mb-2 text-xs font-medium text-muted">This repo only</h3>
      <div className="space-y-3">
        <Field
          id={`${id}-verify`}
          label="Verify commands"
          hint="One per line. They run after the agent resolves or fixes"
        >
          <textarea
            id={`${id}-verify`}
            value={s.verify.join("\n")}
            onChange={(e) => set({ verify: e.target.value ? e.target.value.split("\n") : [] })}
            rows={3}
            placeholder="cargo check"
            className={`${field} h-auto w-full py-2 font-mono`}
          />
        </Field>
        <Field
          id={`${id}-branches`}
          label="Other branches"
          hint="One per line. Each gets a queue, as the default branch has"
        >
          <textarea
            id={`${id}-branches`}
            value={s.branches.join("\n")}
            onChange={(e) => set({ branches: e.target.value ? e.target.value.split("\n") : [] })}
            rows={2}
            placeholder="release"
            className={`${field} h-auto w-full py-2 font-mono`}
          />
        </Field>
      </div>
    </div>
  );
}

/** What a draft changes, line by line. */
function changes(from: RepoSettingsView, to: RepoSettingsView) {
  const fields = Object.keys(to) as (keyof RepoSettingsView)[];
  return fields
    .filter((f) => !isSame(from[f], to[f]))
    .map((f) => [f, JSON.stringify(from[f]), JSON.stringify(to[f])] as const);
}

/** Drafts, on any pane: they leave together, as a PR each, and queue as they open. */
export function DraftsTray({
  workspace,
  drafts,
  discard,
  sent,
}: {
  workspace: number;
  drafts: Drafts;
  discard: () => void;
  /** A draft left as this PR, with the settings it sent. */
  sent: (repo: string, settings: RepoSettingsView, pull: UpdatePullView) => void;
}) {
  const client = useQueryClient();
  const [reviewing, setReviewing] = useState(false);
  const repos = Object.keys(drafts);
  const [errors, setErrors] = useState<Record<string, string>>({});
  // Each repo on its own: one that fails keeps its draft to retry, and the others are done.
  const propose = useMutation({
    mutationFn: async () => {
      setErrors({});
      for (const repo of repos) {
        try {
          const { to } = drafts[repo];
          sent(repo, to, await proposeRepoConfig(workspace, repo, to));
          capture("repo config proposed", { repo });
          client.invalidateQueries({ queryKey: ["repo-config", workspace, repo] });
        } catch (e) {
          const error = e instanceof Error ? e.message : String(e);
          setErrors((errors) => ({ ...errors, [repo]: error }));
        }
      }
    },
  });
  useEffect(() => {
    if (repos.length === 0) setReviewing(false);
  }, [repos.length]);
  if (repos.length === 0) return null;
  const prs = repos.length === 1 ? "a PR" : `${repos.length} PRs`;
  return (
    <>
      <div className="fixed bottom-4 left-1/2 z-30 flex w-[min(40rem,calc(100vw-2rem))] -translate-x-1/2 items-center gap-3 rounded-lg border border-line-strong bg-surface px-4 py-2.5">
        <span className="min-w-0 flex-1 truncate text-xs text-muted">
          Changed {repos.map((r) => r.split("/")[1]).join(", ")}
        </span>
        <button
          type="button"
          onClick={discard}
          className="shrink-0 text-sm text-muted hover:text-fg"
        >
          Discard
        </button>
        <button
          type="button"
          onClick={() => setReviewing(true)}
          className={`${primary} h-8 shrink-0 px-3`}
        >
          Review and open {prs}
        </button>
      </div>
      {reviewing && (
        <div className="fixed inset-0 z-40 flex items-center justify-center bg-black/30 p-6">
          <div className="w-full max-w-xl rounded-lg bg-surface ring-1 ring-line">
            <div className="border-b border-line px-5 py-3">
              <p className="font-medium">Open {prs}</p>
              <p className="text-sm text-muted">
                Each is queued as it opens, so it goes through review and checks like any other
                change, and applies once it merges.
              </p>
            </div>
            <div className="max-h-[50vh] space-y-4 overflow-y-auto px-5 py-4">
              {repos.map((repo) => (
                <div key={repo}>
                  <p className="mb-1 text-sm">{repo}</p>
                  <ul className="rounded-md border border-line bg-page px-3 py-1.5 font-mono text-xs leading-5">
                    {changes(drafts[repo].from, drafts[repo].to).map(([f, a, b]) => (
                      <li key={f} className="break-words">
                        {f}: <span className="text-bad">{a}</span> →{" "}
                        <span className="text-ok">{b}</span>
                      </li>
                    ))}
                  </ul>
                  {errors[repo] && <p className="mt-1 text-sm text-bad">{errors[repo]}</p>}
                </div>
              ))}
            </div>
            <div className="flex items-center justify-end gap-2 border-t border-line px-5 py-3">
              <button
                type="button"
                disabled={propose.isPending}
                onClick={() => setReviewing(false)}
                className={secondary}
              >
                Back
              </button>
              <button
                type="button"
                disabled={propose.isPending}
                onClick={() => propose.mutate()}
                className={primary}
              >
                {propose.isPending
                  ? "Opening…"
                  : `Open ${repos.length === 1 ? "PR" : "PRs"} and queue`}
              </button>
            </div>
          </div>
        </div>
      )}
    </>
  );
}
