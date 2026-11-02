import { useMutation, useQueryClient } from "@tanstack/react-query";
import { useState } from "react";
import { capture } from "./analytics";
import { updateRepo } from "./api";
import type { RepoUpdateView } from "./api/RepoUpdateView";
import type { RepoView } from "./api/RepoView";
import type { UpdatePullView } from "./api/UpdatePullView";
import { RepoIcon, secondary } from "./ui";

type Row =
  | { repo: string; state: "waiting" | "updating" }
  | { repo: string; state: "done"; update: RepoUpdateView }
  | { repo: string; state: "failed"; error: string };

/** One button that brings every repo in `repos` up to the latest settings and agent skill, and
 * what it did to each. One repo at a time, to stay within GitHub's limit on creating content. */
export function UpdateRepos({ workspace, repos }: { workspace: number; repos: RepoView[] }) {
  const queryClient = useQueryClient();
  const [rows, setRows] = useState<Row[]>([]);
  const set = (row: Row) => setRows((rows) => rows.map((r) => (r.repo === row.repo ? row : r)));
  const run = useMutation({
    mutationFn: async () => {
      capture("repos updated", { repos: repos.length });
      setRows(repos.map((r) => ({ repo: r.repo, state: "waiting" })));
      for (const { repo } of repos) {
        set({ repo, state: "updating" });
        try {
          set({ repo, state: "done", update: await updateRepo(workspace, repo) });
        } catch (e) {
          set({ repo, state: "failed", error: e instanceof Error ? e.message : String(e) });
        }
      }
    },
    onSettled: () =>
      Promise.all(
        ["repos", "repo-config", "repo-skill"].map((key) =>
          queryClient.invalidateQueries({ queryKey: [key, workspace] }),
        ),
      ),
  });
  const n = repos.length === 1 ? "1 repo" : `${repos.length} repos`;
  const start = () => {
    if (
      window.confirm(
        `Open and queue the PRs that bring ${n} up to the latest settings and agent skill?`,
      )
    )
      run.mutate();
  };
  return (
    <div className="mb-6 overflow-hidden rounded-lg border border-line bg-surface">
      <div className="flex flex-wrap items-center gap-x-4 gap-y-2 px-5 py-3.5">
        <p className="min-w-0 flex-1 text-muted">
          {run.isPending
            ? `Updating ${rows.filter((r) => r.state === "done" || r.state === "failed").length} of ${rows.length}…`
            : rows.length > 0
              ? summary(rows)
              : `Bring ${n} up to the latest settings and agent skill.`}
        </p>
        <button type="button" onClick={start} disabled={run.isPending} className={secondary}>
          {run.isPending ? "Updating…" : "Update all"}
        </button>
      </div>
      {rows.map((r) => (
        <Outcome key={r.repo} row={r} />
      ))}
    </div>
  );
}

function summary(rows: Row[]) {
  const done = rows.flatMap((r) => (r.state === "done" ? [r.update] : []));
  const pulls = done.flatMap((u) => u.pulls);
  const queued = pulls.filter((p) => p.queued).length;
  const parts = [
    queued > 0 && `${queued} ${queued === 1 ? "PR" : "PRs"} queued`,
    pulls.length > queued && `${pulls.length - queued} not queued`,
    `${done.filter((u) => !u.skipped && u.pulls.length === 0).length} up to date`,
    done.some((u) => u.skipped) && `${done.filter((u) => u.skipped).length} skipped`,
    rows.some((r) => r.state === "failed") &&
      `${rows.filter((r) => r.state === "failed").length} failed`,
  ];
  return parts.filter(Boolean).join(" · ");
}

function Outcome({ row }: { row: Row }) {
  return (
    <div className="flex items-baseline gap-3 border-t border-line-soft px-5 py-2.5">
      <div className="flex w-44 shrink-0 items-center gap-2 self-center">
        <RepoIcon repo={row.repo} size="size-4" />
        <span className="min-w-0 truncate" title={row.repo}>
          {row.repo.split("/")[1]}
        </span>
      </div>
      <div className="min-w-0 flex-1 break-words">
        {row.state === "waiting" && <span className="text-faint">Waiting</span>}
        {row.state === "updating" && <span className="text-muted">Updating…</span>}
        {row.state === "failed" && <span className="text-bad">{row.error}</span>}
        {row.state === "done" &&
          (row.update.skipped ? (
            <span className="text-warn">{row.update.skipped}</span>
          ) : row.update.pulls.length === 0 ? (
            <span className="text-ok">Up to date</span>
          ) : (
            row.update.pulls.map((p) => <Pull key={p.file} pull={p} />)
          ))}
      </div>
    </div>
  );
}

function Pull({ pull: p }: { pull: UpdatePullView }) {
  return (
    <p>
      <a
        href={p.pull_request.url}
        target="_blank"
        rel="noreferrer"
        className="font-medium text-fg underline underline-offset-2"
      >
        {p.file === "settings" ? "Settings" : "Skill"} PR #{p.pull_request.number}
      </a>{" "}
      {p.queued ? (
        <span className="text-ok">queued</span>
      ) : (
        <span className="text-warn">{p.note ?? "not queued"}</span>
      )}
    </p>
  );
}
