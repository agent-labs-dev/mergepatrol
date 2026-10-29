import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import { useState } from "react";
import { fetchRepos, setRepo } from "./api";
import type { AppView } from "./api/AppView";
import type { RepoView } from "./api/RepoView";
import { field, Narrow, secondary } from "./ui";

export function Repos({ workspace, app }: { workspace: number; app: AppView }) {
  const queryClient = useQueryClient();
  const [filter, setFilter] = useState("");
  const repos = useQuery({
    queryKey: ["repos", workspace],
    queryFn: () => fetchRepos(workspace),
  });
  const toggle = useMutation({
    mutationFn: (r: RepoView) => setRepo(workspace, r.repo, { enabled: !r.enabled }),
    onSuccess: () => queryClient.invalidateQueries({ queryKey: ["repos", workspace] }),
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
          <div key={r.repo} className="flex items-center justify-between px-4 py-3">
            <span>
              <span className="font-medium">{r.repo}</span>
              {r.private && <span className="ml-2 text-xs text-faint">Private</span>}
            </span>
            <button
              type="button"
              onClick={() => toggle.mutate(r)}
              disabled={toggle.isPending}
              className={secondary}
            >
              {r.enabled ? "Turn off" : "Turn on"}
            </button>
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
          The repos the App can access. Turn a repo on to run its queue. To add repos,{" "}
          <a href={app.install_url} className="font-medium text-fg underline underline-offset-2">
            change the installation on GitHub
          </a>
          .
        </>
      }
    >
      {repos.isPending && <p className="text-muted">Loading…</p>}
      {repos.error && <p className="text-bad">{repos.error.message}</p>}
      {toggle.error && <p className="text-bad">{toggle.error.message}</p>}
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
            shown.filter((r) => r.enabled),
            "No repo is on.",
          )}
          {group(
            "Available",
            shown.filter((r) => !r.enabled),
            "Every repo is on.",
          )}
        </>
      )}
    </Narrow>
  );
}
