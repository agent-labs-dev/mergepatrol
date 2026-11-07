import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import { type ReactNode, useEffect, useRef, useState } from "react";
import {
  fetchRepoConfig,
  fetchRepoSkill,
  proposeRepoConfig,
  setRepo,
  updateRepoSkill,
} from "./api";
import type { AppView } from "./api/AppView";
import type { RepoSettingsView } from "./api/RepoSettingsView";
import type { RepoView } from "./api/RepoView";
import type { UpdatePullView } from "./api/UpdatePullView";
import { type Drafts, overrides, RepoSettings } from "./RepoSettings";
import { RepoReview } from "./ReviewSettings";
import { field, RepoIcon, secondary } from "./ui";

const small =
  "inline-flex h-7 items-center justify-center rounded-md px-2.5 text-xs font-medium whitespace-nowrap disabled:opacity-40";
const smallPrimary = `${small} bg-fg text-surface hover:bg-fg-2`;
const smallSecondary = `${small} border border-line-strong bg-surface text-fg-2 hover:bg-page`;

/** The repo's settings, which the rail reads for its summary and the form to edit. */
function useConfig(workspace: number, repo: string, enabled = true) {
  return useQuery({
    queryKey: ["repo-config", workspace, repo],
    queryFn: () => fetchRepoConfig(workspace, repo),
    staleTime: 5 * 60_000,
    enabled,
  });
}

/** The workspace, and below it the repos: their summaries say what each sets of its own. */
export function RepoRail({
  workspace,
  app,
  repos,
  error,
  current,
  pick,
  drafts,
}: {
  workspace: number;
  app: AppView;
  repos: RepoView[] | undefined;
  error: Error | null;
  /** None: the workspace is in view. */
  current: string | undefined;
  pick: (repo: string | undefined) => void;
  drafts: Drafts;
}) {
  const [filter, setFilter] = useState("");
  const list = useRef<HTMLElement>(null);
  const shown = repos?.filter((r) => r.repo.includes(filter.trim())) ?? [];
  // A link from elsewhere can pick a repo far down the list: show it, once the list loads.
  const loaded = !!repos;
  useEffect(() => {
    if (loaded && matchMedia("(min-width: 768px)").matches)
      list.current?.querySelector("[aria-current]")?.scrollIntoView({ block: "nearest" });
  }, [loaded]);
  const rows = (title: string, rows: RepoView[], empty: string) => (
    <div className="mb-4">
      <h2 className="px-2 pb-1 text-xs font-medium text-muted">
        {title} · {rows.length}
      </h2>
      {rows.length === 0 && <p className="px-2 py-1 text-faint">{empty}</p>}
      {rows.map((r) => (
        <Item
          key={r.repo}
          workspace={workspace}
          repo={r}
          active={r.repo === current}
          draft={drafts[r.repo]?.to}
          pick={pick}
        />
      ))}
    </div>
  );
  return (
    <aside
      ref={list}
      className="border-line px-3 py-4 md:h-full md:overflow-y-auto md:border-r md:pb-16"
    >
      <button
        type="button"
        onClick={() => pick(undefined)}
        aria-current={current ? undefined : true}
        className={`mb-4 w-full rounded-md px-2 py-2 text-left ${current ? "hover:bg-sunken/60" : "bg-sunken"}`}
      >
        <span className="block font-medium">Workspace</span>
        <span className="block text-xs text-muted">Plan, models, and defaults for every repo</span>
      </button>
      <input
        value={filter}
        onChange={(e) => setFilter(e.target.value)}
        placeholder="Filter repos"
        className={`${field} mb-3 w-full`}
      />
      {!repos && !error && <p className="px-2 text-muted">Loading…</p>}
      {error && <p className="px-2 text-bad">{error.message}</p>}
      {repos?.length === 0 && (
        <p className="px-2 text-muted">The App cannot access any repos yet.</p>
      )}
      {!!repos?.length && (
        <>
          {rows(
            "Queuing",
            shown.filter((r) => r.on),
            "No repo is on.",
          )}
          {rows(
            "Available",
            shown.filter((r) => !r.on),
            "Every repo is on.",
          )}
        </>
      )}
      <a
        href={app.install_url}
        className="block px-2 pt-1 text-xs text-muted underline underline-offset-2 hover:text-fg"
      >
        Add repos on GitHub
      </a>
    </aside>
  );
}

function Item({
  workspace,
  repo: r,
  active,
  draft,
  pick,
}: {
  workspace: number;
  repo: RepoView;
  active: boolean;
  draft: RepoSettingsView | undefined;
  pick: (repo: string) => void;
}) {
  const skill = useSkill(workspace, r);
  const config = useConfig(workspace, r.repo, r.has_config);
  const c = config.data;
  const s = skill.data;
  const needs =
    r.on && r.config_error
      ? { tone: "bg-bad", label: "The config file does not parse" }
      : r.on && !r.has_config
        ? { tone: "bg-warn", label: "No config file" }
        : s && s.state !== "current" && !s.pull_request_current
          ? { tone: "bg-warn", label: s.state === "missing" ? "No agent skill" : "Skill outdated" }
          : null;
  const own = c ? overrides(draft ?? c.settings, c.defaults, c.providers) : [];
  return (
    <button
      type="button"
      onClick={() => pick(r.repo)}
      aria-current={active || undefined}
      className={`flex w-full items-start gap-2 rounded-md px-2 py-1.5 text-left ${active ? "bg-sunken text-fg" : "text-fg-2 hover:bg-sunken/60"}`}
    >
      <span className="pt-0.5">
        <RepoIcon repo={r.repo} size="size-4" />
      </span>
      <span className="min-w-0 flex-1">
        <span className="flex items-center gap-2">
          <span className="min-w-0 flex-1 truncate" title={r.repo}>
            {r.repo.split("/")[1]}
          </span>
          {draft && <span className="text-[11px] text-warn">edited</span>}
          {needs && (
            <span className={`size-1.5 shrink-0 rounded-full ${needs.tone}`} title={needs.label} />
          )}
        </span>
        {(r.on || own.length > 0) && (
          <span
            className={`block truncate text-xs ${own.length ? "text-muted" : "text-faint"}`}
            title={own.join(" · ")}
          >
            {own.join(" · ") ||
              (c ? (c.follows ? "Follows the workspace" : "Matches the workspace") : "\u00a0")}
          </span>
        )}
      </span>
    </button>
  );
}

/** The repo's `mergeloop` agent skill, which only a repo with a config file is checked for. */
function useSkill(workspace: number, repo: RepoView) {
  return useQuery({
    queryKey: ["repo-skill", workspace, repo.repo],
    queryFn: () => fetchRepoSkill(workspace, repo.repo),
    staleTime: 5 * 60_000,
    enabled: repo.has_config,
  });
}

/** The repos that set something of their own, for the workspace's view of them. */
export function DifferingRepos({
  workspace,
  repos,
  pick,
}: {
  workspace: number;
  repos: RepoView[];
  pick: (repo: string) => void;
}) {
  return (
    <div className="divide-y divide-line-soft rounded-lg border border-line bg-surface">
      {repos.map((r) => (
        <Differing key={r.repo} workspace={workspace} repo={r} pick={pick} />
      ))}
    </div>
  );
}

function Differing({
  workspace,
  repo: r,
  pick,
}: {
  workspace: number;
  repo: RepoView;
  pick: (repo: string) => void;
}) {
  const { data: c } = useConfig(workspace, r.repo, r.has_config);
  const own = c ? overrides(c.settings, c.defaults, c.providers) : [];
  if (own.length === 0) return null;
  return (
    <button
      type="button"
      onClick={() => pick(r.repo)}
      className="flex w-full items-center gap-3 px-4 py-2.5 text-left hover:bg-page"
    >
      <RepoIcon repo={r.repo} size="size-5" />
      <span className="w-40 shrink-0 truncate font-medium">{r.repo.split("/")[1]}</span>
      <span className="min-w-0 flex-1 truncate text-xs text-fg-2">{own.join(" · ")}</span>
    </button>
  );
}

/** A repo: its queue, config file, skill and settings. An edit goes to `edit`, not the repo. */
export function RepoDetail({
  workspace,
  repo: r,
  draft,
  edit,
  opened,
}: {
  workspace: number;
  repo: RepoView;
  draft: RepoSettingsView | undefined;
  edit: (from: RepoSettingsView, to: RepoSettingsView) => void;
  /** The PR the page opened from a draft. */
  opened: UpdatePullView | undefined;
}) {
  const queryClient = useQueryClient();
  const config = useConfig(workspace, r.repo);
  const override = useMutation({
    mutationFn: (enabled: boolean | null) => setRepo(workspace, r.repo, { enabled }),
    onSuccess: () => queryClient.invalidateQueries({ queryKey: ["repos", workspace] }),
  });
  const c = config.data;
  const settings = c && (draft ?? c.settings);
  return (
    <div className="overflow-hidden rounded-lg border border-line bg-surface">
      <div className="flex items-center gap-3 px-5 py-4">
        <RepoIcon repo={r.repo} size="size-8" />
        <div className="min-w-0">
          <h2 className="text-base font-semibold break-words">{r.repo}</h2>
          <p className="text-xs text-muted">
            {r.private ? "Private" : "Public"} · {r.on ? "Queuing" : "Not queuing"}
          </p>
        </div>
        <a
          href={`https://github.com/${r.repo}`}
          target="_blank"
          rel="noreferrer"
          className={`${secondary} ml-auto shrink-0`}
        >
          Open on GitHub
        </a>
      </div>
      <Fact
        label="Queue"
        action={
          <>
            {r.enabled !== null && (
              <button
                type="button"
                onClick={() => override.mutate(null)}
                disabled={override.isPending}
                className={smallSecondary}
              >
                Follow config file
              </button>
            )}
            <Switch
              on={r.on}
              onChange={(on) => override.mutate(on)}
              disabled={override.isPending}
              label={`Queue ${r.repo}`}
            />
          </>
        }
      >
        {r.on ? "On" : "Off"}{" "}
        <span className="text-muted">
          ·{" "}
          {r.enabled === null
            ? "follows the config file"
            : "set here, whatever the config file says"}
        </span>
        {override.error && <p className="text-xs text-bad">{override.error.message}</p>}
      </Fact>
      <ConfigFact workspace={workspace} repo={r} />
      <SkillFact workspace={workspace} repo={r} />
      {opened && (
        <Fact label="Settings PR">
          <a
            href={opened.pull_request.url}
            target="_blank"
            rel="noreferrer"
            className="font-medium underline underline-offset-2"
          >
            PR #{opened.pull_request.number}
          </a>{" "}
          {opened.queued ? (
            <span className="text-ok">queued</span>
          ) : (
            <span className="text-warn">{opened.note ?? "not queued"}</span>
          )}
        </Fact>
      )}
      {config.isPending && <p className="border-t border-line-soft p-5 text-muted">Loading…</p>}
      {config.error && (
        <p className="border-t border-line-soft p-5 text-bad">{config.error.message}</p>
      )}
      {c && settings && (
        <RepoSettings
          config={c}
          settings={settings}
          onChange={(to) => edit(c.settings, to)}
          review={
            <RepoReview
              workspace={workspace}
              review={settings.review}
              saved={c.settings.review}
              onChange={(review) => edit(c.settings, { ...settings, review })}
            />
          }
        />
      )}
    </div>
  );
}

function Fact({
  label,
  action,
  children,
}: {
  label: string;
  action?: ReactNode;
  children: ReactNode;
}) {
  return (
    <div className="flex flex-wrap items-center gap-x-4 gap-y-2 border-t border-line-soft px-5 py-3.5">
      <div className="w-24 shrink-0 text-xs text-muted">{label}</div>
      <div className="min-w-0 flex-1 break-words">{children}</div>
      {action && <div className="flex shrink-0 items-center gap-3">{action}</div>}
    </div>
  );
}

function Switch({
  on,
  onChange,
  disabled,
  label,
}: {
  on: boolean;
  onChange: (on: boolean) => void;
  disabled: boolean;
  label: string;
}) {
  return (
    <button
      type="button"
      role="switch"
      aria-checked={on}
      aria-label={label}
      title={on ? "Turn off" : "Turn on"}
      onClick={() => onChange(!on)}
      disabled={disabled}
      className={`relative h-5 w-9 shrink-0 rounded-full transition-colors disabled:opacity-40 ${on ? "bg-ok" : "bg-line-strong"}`}
    >
      <span
        className={`absolute top-0.5 size-4 rounded-full bg-white shadow transition-all ${on ? "left-4.5" : "left-0.5"}`}
      />
    </button>
  );
}

/** The config file, or a button that opens a PR adding it with the defaults. */
function ConfigFact({ workspace, repo: r }: { workspace: number; repo: RepoView }) {
  const queryClient = useQueryClient();
  // The settings form below reads the same query, so this costs no extra request.
  const config = useConfig(workspace, r.repo);
  const setUp = useMutation({
    mutationFn: async () => {
      const c = await fetchRepoConfig(workspace, r.repo);
      return (
        c.pull_request ?? (await proposeRepoConfig(workspace, r.repo, c.defaults)).pull_request
      );
    },
    onSuccess: () =>
      queryClient.invalidateQueries({ queryKey: ["repo-config", workspace, r.repo] }),
  });
  const pr = setUp.data ?? config.data?.pull_request;
  return (
    <Fact
      label="Config file"
      action={
        !r.has_config &&
        (pr ? (
          <a href={pr.url} target="_blank" rel="noreferrer" className={smallSecondary}>
            Setup PR #{pr.number}
          </a>
        ) : (
          <button
            type="button"
            onClick={() => setUp.mutate()}
            disabled={setUp.isPending}
            title={setUp.error?.message ?? "Open a PR that adds the config file with the defaults"}
            className={smallPrimary}
          >
            {setUp.isPending ? "Opening PR…" : setUp.error ? "Retry set up" : "Set up"}
          </button>
        ))
      }
    >
      {r.config_error ? (
        <span className="text-bad" title={r.config_error}>
          Does not parse{r.on && ", so the queue is paused"}
        </span>
      ) : r.has_config ? (
        config.data ? (
          <code className="rounded bg-sunken px-1 text-xs">{config.data.path}</code>
        ) : (
          <span className="text-muted">On the default branch</span>
        )
      ) : (
        <span className="text-muted">None on the default branch</span>
      )}
    </Fact>
  );
}

/** The agent skill: up to date, or a button that opens a PR writing the current one, or the PR
 * open for it. */
function SkillFact({ workspace, repo: r }: { workspace: number; repo: RepoView }) {
  const queryClient = useQueryClient();
  const skill = useSkill(workspace, r);
  const update = useMutation({
    mutationFn: () => updateRepoSkill(workspace, r.repo),
    onSuccess: () => queryClient.invalidateQueries({ queryKey: ["repo-skill", workspace, r.repo] }),
  });
  const s = skill.data;
  if (!r.has_config)
    return (
      <Fact label="Agent skill">
        <span className="text-faint">Checked once the repo has a config file</span>
      </Fact>
    );
  if (!s)
    return (
      <Fact label="Agent skill">
        <span className={skill.error ? "text-bad" : "text-faint"}>
          {skill.error?.message ?? "Checking…"}
        </span>
      </Fact>
    );
  const link = s.pull_request && (
    <a href={s.pull_request.url} target="_blank" rel="noreferrer" className={smallSecondary}>
      Skill PR #{s.pull_request.number}
    </a>
  );
  return (
    <Fact
      label="Agent skill"
      action={
        // The open PR has the current skill: merging it is all that is left.
        s.state !== "current" &&
        (link && s.pull_request_current ? (
          link
        ) : (
          <>
            {link}
            <button
              type="button"
              onClick={() => update.mutate()}
              disabled={update.isPending}
              title={update.error?.message}
              className={smallPrimary}
            >
              {update.isPending
                ? "Opening PR…"
                : update.error
                  ? "Retry"
                  : s.state === "missing"
                    ? "Add skill"
                    : link
                      ? "Update PR"
                      : "Update skill"}
            </button>
          </>
        ))
      }
    >
      {s.state === "current" ? (
        <span className="text-ok">Up to date</span>
      ) : (
        <span className="text-warn">
          {s.state === "missing" ? "Not in the repo" : "Older than MergeLoop's"}
        </span>
      )}
      {update.data && (
        <p className="text-xs text-muted">
          {update.data.queued ? "Queued." : (update.data.note ?? "Not queued.")}
        </p>
      )}
    </Fact>
  );
}
