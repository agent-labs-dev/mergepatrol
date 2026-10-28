import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import { fetchRepos, setRepo } from "./api";
import type { AppView } from "./api/AppView";
import type { RepoView } from "./api/RepoView";

export function Repos({ workspace, app }: { workspace: number; app: AppView }) {
  const queryClient = useQueryClient();
  const repos = useQuery({
    queryKey: ["repos", workspace],
    queryFn: () => fetchRepos(workspace),
  });
  const toggle = useMutation({
    mutationFn: (r: RepoView) => setRepo(workspace, r.repo, { enabled: !r.enabled }),
    onSuccess: () => queryClient.invalidateQueries({ queryKey: ["repos", workspace] }),
  });

  return (
    <section>
      <p className="mb-4 text-sm text-neutral-400">
        The repos the App can access. Turn a repo on to run its queue. To add repos,{" "}
        <a className="text-sky-400 underline" href={app.install_url}>
          change the installation on GitHub
        </a>
        .
      </p>
      {repos.isPending && <p className="text-neutral-500">Loading…</p>}
      {repos.error && <p className="text-red-400">{repos.error.message}</p>}
      {toggle.error && <p className="text-red-400">{toggle.error.message}</p>}
      {repos.data?.length === 0 && (
        <p className="text-neutral-400">The App cannot access any repos yet.</p>
      )}
      <ul className="divide-y divide-neutral-800 rounded border border-neutral-800">
        {repos.data?.map((r) => (
          <li key={r.repo} className="flex items-center justify-between p-3">
            <span className="font-mono text-sm">
              {r.repo}
              {r.private && <span className="ml-2 text-xs text-neutral-500">private</span>}
            </span>
            <button
              type="button"
              onClick={() => toggle.mutate(r)}
              disabled={toggle.isPending}
              className={`rounded px-3 py-1 text-xs ${
                r.enabled ? "bg-emerald-800 text-emerald-100" : "bg-neutral-800 text-neutral-300"
              }`}
            >
              {r.enabled ? "On" : "Off"}
            </button>
          </li>
        ))}
      </ul>
    </section>
  );
}
