import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import { useNavigate, useSearch } from "@tanstack/react-router";
import { useState } from "react";
import { fetchRepoConfig, fetchRepos, proposeRepoConfig, setRepo } from "./api";
import type { AppView } from "./api/AppView";
import type { PullRequestRef } from "./api/PullRequestRef";
import type { RepoView } from "./api/RepoView";
import { RepoSettings } from "./RepoSettings";
import { field, Narrow, primary, secondary } from "./ui";

export function Repos({ workspace, app }: { workspace: number; app: AppView }) {
  const queryClient = useQueryClient();
  const [filter, setFilter] = useState("");
  const { repo: open } = useSearch({ from: "/$account/repos" });
  const navigate = useNavigate({ from: "/$account/repos" });
  const repos = useQuery({
    queryKey: ["repos", workspace],
    queryFn: () => fetchRepos(workspace),
  });
  const refresh = () => queryClient.invalidateQueries({ queryKey: ["repos", workspace] });
  const override = useMutation({
    mutationFn: ({ repo, enabled }: { repo: string; enabled: boolean | null }) =>
      setRepo(workspace, repo, { enabled }),
    onSuccess: refresh,
  });
  const shown = repos.data?.filter((r) => r.repo.includes(filter.trim())) ?? [];

  const group = (title: string, rows: RepoView[], empty: string) => (
    <section className="mt-6">
      <h2 className="mb-2 text-xs font-medium text-muted">
        {title} · {rows.length}
      </h2>
      <div className="divide-y divide-line-soft overflow-hidden rounded-lg border border-line bg-surface">
        {rows.length === 0 && <p className="px-4 py-3 text-faint">{empty}</p>}
        {rows.map((r) => (
          <div key={r.repo}>
            <div className="flex flex-wrap items-center justify-between gap-3 px-4 py-3">
              <span className="min-w-0 break-words">
                <span className="font-medium">{r.repo}</span>
                {r.private && <span className="ml-2 text-xs text-faint">Private</span>}
                <Status repo={r} />
              </span>
              <span className="flex shrink-0 gap-2">
                {!r.has_config && <SetUp workspace={workspace} repo={r.repo} />}
                <button
                  type="button"
                  onClick={() => navigate({ search: open === r.repo ? {} : { repo: r.repo } })}
                  className={secondary}
                >
                  Settings
                </button>
                <button
                  type="button"
                  onClick={() => override.mutate({ repo: r.repo, enabled: !r.on })}
                  disabled={override.isPending}
                  className={secondary}
                >
                  {r.on ? "Turn off" : "Turn on"}
                </button>
              </span>
            </div>
            {r.enabled !== null && (
              <p className="-mt-2 px-4 pb-3 text-xs text-muted">
                Turned {r.enabled ? "on" : "off"} here, whatever the config file says.{" "}
                <button
                  type="button"
                  onClick={() => override.mutate({ repo: r.repo, enabled: null })}
                  className="font-medium text-fg underline underline-offset-2"
                >
                  Follow the config file
                </button>
              </p>
            )}
            {open === r.repo && <RepoSettings workspace={workspace} repo={r.repo} />}
          </div>
        ))}
      </div>
    </section>
  );

  return (
    <Narrow
      title="Repos"
      sub={
        <>
          The repos the App can access. A repo queues once its default branch has{" "}
          <code className="rounded bg-sunken px-1 text-xs">.mergepatrol/mergepatrol.yml</code>: set
          it up to open a PR that adds it. To add repos,{" "}
          <a href={app.install_url} className="font-medium text-fg underline underline-offset-2">
            change the installation on GitHub
          </a>
          .
        </>
      }
    >
      {repos.isPending && <p className="text-muted">Loading…</p>}
      {repos.error && <p className="text-bad">{repos.error.message}</p>}
      {override.error && <p className="text-bad">{override.error.message}</p>}
      {repos.data?.length === 0 && (
        <p className="text-muted">The App cannot access any repos yet.</p>
      )}
      {!!repos.data?.length && (
        <>
          <input
            value={filter}
            onChange={(e) => setFilter(e.target.value)}
            placeholder="Filter repos"
            className={`${field} w-full`}
          />
          {group(
            "Queuing",
            shown.filter((r) => r.on),
            "No repo is on.",
          )}
          {group(
            "Available",
            shown.filter((r) => !r.on),
            "Every repo is on.",
          )}
        </>
      )}
    </Narrow>
  );
}

function Status({ repo }: { repo: RepoView }) {
  if (repo.on && repo.config_error)
    return (
      <span className="mt-0.5 block text-xs text-warn" title={repo.config_error}>
        Paused: the config file does not parse. {repo.config_error}
      </span>
    );
  if (!repo.has_config)
    return <span className="mt-0.5 block text-xs text-faint">No config file</span>;
  return null;
}

/** Opens a PR that adds the config file with the defaults. */
function SetUp({ workspace, repo }: { workspace: number; repo: string }) {
  const queryClient = useQueryClient();
  const [pr, setPr] = useState<PullRequestRef | null>(null);
  const setUp = useMutation({
    mutationFn: async () => {
      const config = await fetchRepoConfig(workspace, repo);
      return config.pull_request ?? proposeRepoConfig(workspace, repo, config.defaults);
    },
    onSuccess: (opened) => {
      setPr(opened);
      queryClient.invalidateQueries({ queryKey: ["repo-config", workspace, repo] });
    },
  });
  if (pr)
    return (
      <a href={pr.url} target="_blank" rel="noreferrer" className={secondary}>
        Setup PR #{pr.number}
      </a>
    );
  return (
    <button
      type="button"
      onClick={() => setUp.mutate()}
      disabled={setUp.isPending}
      title={setUp.error?.message}
      className={primary}
    >
      {setUp.isPending ? "Opening PR…" : setUp.error ? "Retry set up" : "Set up"}
    </button>
  );
}
