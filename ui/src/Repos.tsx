import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import { useNavigate, useSearch } from "@tanstack/react-router";
import { type ReactNode, useEffect, useRef, useState } from "react";
import {
  fetchRepoConfig,
  fetchRepoSkill,
  fetchRepos,
  proposeRepoConfig,
  setRepo,
  updateRepoSkill,
} from "./api";
import type { AppView } from "./api/AppView";
import type { RepoView } from "./api/RepoView";
import { RepoSettings } from "./RepoSettings";
import { field, Narrow, RepoIcon, secondary } from "./ui";

const small =
  "inline-flex h-7 items-center justify-center rounded-md px-2.5 text-xs font-medium whitespace-nowrap disabled:opacity-40";
const smallPrimary = `${small} bg-fg text-surface hover:bg-fg-2`;
const smallSecondary = `${small} border border-line-strong bg-surface text-fg-2 hover:bg-page`;

/** The repos on the left, the one picked on the right: its queue, config file, skill and
 * settings. */
export function Repos({ workspace, app }: { workspace: number; app: AppView }) {
  // "Turn on MergeLoop" and permalinks link here with a repo, which picks it.
  const { repo: picked } = useSearch({ from: "/$account/repos" });
  const navigate = useNavigate({ from: "/$account/repos" });
  const [filter, setFilter] = useState("");
  const repos = useQuery({
    queryKey: ["repos", workspace],
    queryFn: () => fetchRepos(workspace),
  });
  const detail = useRef<HTMLElement>(null);
  const list = useRef<HTMLElement>(null);
  const shown = repos.data?.filter((r) => r.repo.includes(filter.trim())) ?? [];
  const current = repos.data?.find((r) => r.repo === picked) ?? shown[0] ?? repos.data?.[0];
  const pick = (repo: string) => {
    navigate({ search: { repo }, replace: true });
    // One column on a phone: the detail is below the list, out of view.
    if (matchMedia("(max-width: 767px)").matches) detail.current?.scrollIntoView();
  };
  // A link from elsewhere can pick a repo far down the list: show it, once the list loads.
  const loaded = !!repos.data;
  useEffect(() => {
    if (loaded && matchMedia("(min-width: 768px)").matches)
      list.current?.querySelector("[aria-current]")?.scrollIntoView({ block: "nearest" });
  }, [loaded]);

  return (
    <Narrow
      wide
      title="Repos"
      sub={
        <>
          The repos the App can access. A repo queues once its default branch has{" "}
          <code className="rounded bg-sunken px-1 text-xs">.mergepatrol/mergepatrol.yml</code>. To
          add repos,{" "}
          <a href={app.install_url} className="font-medium text-fg underline underline-offset-2">
            change the installation on GitHub
          </a>
          .
        </>
      }
    >
      {repos.isPending && <p className="text-muted">Loading…</p>}
      {repos.error && <p className="text-bad">{repos.error.message}</p>}
      {repos.data?.length === 0 && (
        <p className="text-muted">The App cannot access any repos yet.</p>
      )}
      {current && (
        <div className="grid gap-6 md:grid-cols-[16rem_1fr]">
          <aside
            ref={list}
            className="md:sticky md:top-4 md:max-h-[calc(100dvh-8rem)] md:self-start md:overflow-y-auto"
          >
            <input
              value={filter}
              onChange={(e) => setFilter(e.target.value)}
              placeholder="Filter repos"
              className={`${field} mb-3 w-full`}
            />
            <List
              title="Queuing"
              rows={shown.filter((r) => r.on)}
              empty="No repo is on."
              workspace={workspace}
              current={current.repo}
              pick={pick}
            />
            <List
              title="Available"
              rows={shown.filter((r) => !r.on)}
              empty="Every repo is on."
              workspace={workspace}
              current={current.repo}
              pick={pick}
            />
          </aside>
          <section ref={detail} className="min-w-0 scroll-mt-4">
            <Detail key={current.repo} workspace={workspace} repo={current} />
          </section>
        </div>
      )}
    </Narrow>
  );
}

function List({
  title,
  rows,
  empty,
  workspace,
  current,
  pick,
}: {
  title: string;
  rows: RepoView[];
  empty: string;
  workspace: number;
  current: string;
  pick: (repo: string) => void;
}) {
  return (
    <div className="mb-4">
      <h2 className="px-2 pb-1 text-xs font-medium text-muted">
        {title} · {rows.length}
      </h2>
      {rows.length === 0 && <p className="px-2 py-1 text-faint">{empty}</p>}
      {rows.map((r) => (
        <Item key={r.repo} workspace={workspace} repo={r} active={r.repo === current} pick={pick} />
      ))}
    </div>
  );
}

function Item({
  workspace,
  repo: r,
  active,
  pick,
}: {
  workspace: number;
  repo: RepoView;
  active: boolean;
  pick: (repo: string) => void;
}) {
  const skill = useSkill(workspace, r);
  const s = skill.data;
  const needs =
    r.on && r.config_error
      ? { tone: "bg-bad", label: "The config file does not parse" }
      : r.on && !r.has_config
        ? { tone: "bg-warn", label: "No config file" }
        : s && s.state !== "current" && !s.pull_request_current
          ? { tone: "bg-warn", label: s.state === "missing" ? "No agent skill" : "Skill outdated" }
          : null;
  return (
    <button
      type="button"
      onClick={() => pick(r.repo)}
      aria-current={active || undefined}
      className={`flex w-full items-center gap-2 rounded-md px-2 py-1.5 text-left ${active ? "bg-sunken text-fg" : "text-fg-2 hover:bg-sunken/60"}`}
    >
      <RepoIcon repo={r.repo} size="size-4" />
      <span className="min-w-0 flex-1 truncate" title={r.repo}>
        {r.repo.split("/")[1]}
      </span>
      {needs && (
        <span className={`size-1.5 shrink-0 rounded-full ${needs.tone}`} title={needs.label} />
      )}
    </button>
  );
}

/** The repo's `mergepatrol` agent skill, which only a repo with a config file is checked for. */
function useSkill(workspace: number, repo: RepoView) {
  return useQuery({
    queryKey: ["repo-skill", workspace, repo.repo],
    queryFn: () => fetchRepoSkill(workspace, repo.repo),
    staleTime: 5 * 60_000,
    enabled: repo.has_config,
  });
}

function Detail({ workspace, repo: r }: { workspace: number; repo: RepoView }) {
  const queryClient = useQueryClient();
  const override = useMutation({
    mutationFn: (enabled: boolean | null) => setRepo(workspace, r.repo, { enabled }),
    onSuccess: () => queryClient.invalidateQueries({ queryKey: ["repos", workspace] }),
  });
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
      <RepoSettings workspace={workspace} repo={r.repo} />
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
  const config = useQuery({
    queryKey: ["repo-config", workspace, r.repo],
    queryFn: () => fetchRepoConfig(workspace, r.repo),
  });
  const setUp = useMutation({
    mutationFn: async () => {
      const c = await fetchRepoConfig(workspace, r.repo);
      return c.pull_request ?? proposeRepoConfig(workspace, r.repo, c.defaults);
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
        <code className="rounded bg-sunken px-1 text-xs">.mergepatrol/mergepatrol.yml</code>
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
    </Fact>
  );
}
