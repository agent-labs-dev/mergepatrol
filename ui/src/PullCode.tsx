import { type FileDiffMetadata, parsePatchFiles } from "@pierre/diffs";
import { FileDiff } from "@pierre/diffs/react";
import type { GitStatus } from "@pierre/trees";
import { FileTree, useFileTree } from "@pierre/trees/react";
import { useQuery } from "@tanstack/react-query";
import { type CSSProperties, type ReactNode, useMemo, useState } from "react";
import { fetchPullDiff } from "./api";
import { useDark } from "./theme";

/** A changed file, with its lines added and removed. */
type ChangedFile = FileDiffMetadata & { additions: number; deletions: number };

export type Commit = { sha: string; headline: string };

/** A PR's Code tab: each changed file a row that opens to its diff, as GitHub's "Files changed".
 * Viewed folds a file away and counts it. The tree lays the files out by folder beside them.
 * Its own chunk: the highlighter and the tree are most of its weight. */
export default function PullCode({
  workspace,
  repo,
  number,
  commits,
}: {
  workspace: number;
  repo: string;
  number: number;
  /** The PR's commits, oldest first, to show one at a time. */
  commits: Commit[];
}) {
  const [commit, setCommit] = useState<string>();
  const diff = useQuery({
    queryKey: ["pull-diff", workspace, repo, number, commit],
    queryFn: () => fetchPullDiff(workspace, repo, number, commit),
    staleTime: 30_000,
  });
  const files = useMemo(
    () =>
      parsePatchFiles(diff.data?.patch ?? "")
        .flatMap((p) => p.files)
        .map((f) => ({
          ...f,
          additions: f.hunks.reduce((n, h) => n + h.additionLines, 0),
          deletions: f.hunks.reduce((n, h) => n + h.deletionLines, 0),
        })),
    [diff.data],
  );
  const [open, setOpen] = useState(new Set<string>());
  const [viewed, setViewed] = useState(new Set<string>());
  const [split, setSplit] = useState(false);
  const [tree, setTree] = useState(false);
  const allOpen = files.length > 0 && files.every((f) => open.has(f.name));
  const reveal = (path: string) => {
    setOpen((o) => toggled(o, path, true));
    requestAnimationFrame(() =>
      document.getElementById(anchor(path))?.scrollIntoView({ block: "start" }),
    );
  };
  return (
    <div className="flex min-h-0 flex-1 flex-col">
      <div className="flex items-center gap-3 border-b border-line px-4 py-2 sm:px-6">
        <select
          value={commit ?? ""}
          onChange={(e) => setCommit(e.target.value || undefined)}
          className="h-8 w-auto max-w-48 shrink-0 truncate rounded-md border-0 bg-sunken pl-3 text-sm"
        >
          <option value="">All commits</option>
          {commits.map((c) => (
            <option key={c.sha} value={c.sha}>
              {c.sha.slice(0, 7)} {c.headline}
            </option>
          ))}
        </select>
        {diff.data && (
          <span className="truncate text-sm whitespace-nowrap text-muted tabular-nums">
            {files.length} {files.length === 1 ? "file" : "files"} · {viewedOf(files, viewed)} /{" "}
            {files.length} viewed
          </span>
        )}
        <span className="ml-auto flex items-center gap-0.5">
          <Tool
            label={split ? "Unified diff" : "Split diff"}
            on={split}
            onClick={() => setSplit((v) => !v)}
            d="M2.5 4.5h7M12.5 4.5h1M2.5 11.5h1M6.5 11.5h7M11 3v3M5 10v3"
          />
          <Tool
            label={allOpen ? "Collapse all" : "Expand all"}
            onClick={() => setOpen(allOpen ? new Set() : new Set(files.map((f) => f.name)))}
            d={allOpen ? "M5 3l3 3 3-3M5 13l3-3 3 3" : "M5 6l3-3 3 3M5 10l3 3 3-3"}
          />
          <Tool
            label="File tree"
            on={tree}
            onClick={() => setTree((t) => !t)}
            d="M3.5 2.5v9h4M3.5 6.5h4M9.5 4.5h4v4h-4zM9.5 9.5h4v4h-4z"
          />
        </span>
      </div>
      {diff.isPending && <p className="px-4 py-6 text-faint sm:px-6">Loading the changes…</p>}
      {diff.isError && <p className="px-4 py-6 text-bad sm:px-6">{diff.error.message}</p>}
      {diff.data && (
        <div
          className={`min-h-0 flex-1 ${tree ? "grid grid-cols-[minmax(180px,35%)_1fr]" : "flex flex-col"}`}
        >
          {/* Keyed by the files: the tree reads its paths once. */}
          {tree && <Tree key={commit ?? "all"} files={files} onSelect={reveal} />}
          <ul className="min-h-0 overflow-y-auto">
            {files.map((f) => (
              <FileRow
                key={f.name}
                file={f}
                open={open.has(f.name)}
                viewed={viewed.has(f.name)}
                // The tree beside shows the folders.
                short={tree}
                split={split}
                onToggle={() => setOpen((o) => toggled(o, f.name))}
                onViewed={(on) => {
                  setViewed((v) => toggled(v, f.name, on));
                  // GitHub's way: a file marked viewed folds away.
                  if (on) setOpen((o) => toggled(o, f.name, false));
                }}
              />
            ))}
          </ul>
        </div>
      )}
    </div>
  );
}

/** A file's identity across commits: viewed in one, viewed in all, as on GitHub. */
const viewedOf = (files: ChangedFile[], viewed: Set<string>) =>
  files.filter((f) => viewed.has(f.name)).length;

const toggled = (set: Set<string>, key: string, on = !set.has(key)) => {
  const next = new Set(set);
  if (on) next.add(key);
  else next.delete(key);
  return next;
};

const anchor = (path: string) => `file-${path}`;

function FileRow({
  file: f,
  open,
  viewed,
  short,
  split,
  onToggle,
  onViewed,
}: {
  file: ChangedFile;
  open: boolean;
  viewed: boolean;
  /** The file's name alone, not its path. */
  short: boolean;
  split: boolean;
  onToggle: () => void;
  onViewed: (on: boolean) => void;
}) {
  const dark = useDark();
  return (
    <li id={anchor(f.name)} className="border-b border-line-soft">
      {/* Sticks while its diff scrolls past, so Viewed is always in reach. */}
      <div
        className={`sticky top-0 z-10 flex items-center gap-3 bg-surface px-4 py-2.5 text-sm sm:px-6 ${viewed ? "opacity-60" : ""}`}
      >
        <button
          type="button"
          onClick={onToggle}
          aria-expanded={open}
          className="flex min-w-0 flex-1 items-center gap-3 text-left"
        >
          <span className={`text-faint transition-transform ${open ? "rotate-90" : ""}`}>
            <Icon d="M6 4l4 4-4 4" />
          </span>
          <FileStatus type={f.type} />
          <span className="truncate" title={f.name}>
            {short
              ? f.name.split("/").at(-1)
              : `${f.prevName && f.prevName !== f.name ? `${f.prevName} → ` : ""}${f.name}`}
          </span>
        </button>
        <span className="font-mono tabular-nums">
          <span className="text-ok">+{f.additions}</span>{" "}
          <span className="text-bad">−{f.deletions}</span>
        </span>
        <label className="flex cursor-pointer items-center gap-2 text-muted">
          <input
            type="checkbox"
            checked={viewed}
            onChange={(e) => onViewed(e.target.checked)}
            className="size-4 accent-fg"
          />
          Viewed
        </label>
      </div>
      {open && (
        <div className="px-4 pb-4 sm:px-6">
          <FileDiff
            fileDiff={f}
            className="block overflow-hidden rounded-md border border-line"
            style={{ "--diffs-font-size": "12px" } as CSSProperties}
            options={{
              diffStyle: split ? "split" : "unified",
              themeType: dark ? "dark" : "light",
              overflow: split ? "scroll" : "wrap",
              hunkSeparators: "line-info",
              lineDiffType: "word",
              disableFileHeader: true,
            }}
          />
        </div>
      )}
    </li>
  );
}

const gitStatus: Record<FileDiffMetadata["type"], GitStatus> = {
  change: "modified",
  new: "added",
  deleted: "deleted",
  "rename-pure": "renamed",
  "rename-changed": "renamed",
};

/** The tree in the dashboard's colors. */
const treeColors = {
  "--trees-bg-override": "transparent",
  "--trees-fg-override": "var(--fg)",
  "--trees-fg-muted-override": "var(--muted)",
  "--trees-selected-bg-override": "var(--sunken)",
  "--trees-selected-fg-override": "var(--fg)",
  "--trees-border-color-override": "var(--line)",
  "--trees-bg-muted-override": "var(--page)",
  "--trees-search-bg-override": "var(--surface)",
  "--trees-search-fg-override": "var(--fg)",
  "--trees-input-bg-override": "var(--surface)",
  "--trees-font-size-override": "12px",
  "--trees-git-added-color-override": "var(--ok)",
  "--trees-git-deleted-color-override": "var(--bad)",
  "--trees-git-modified-color-override": "var(--warn)",
  colorScheme: "inherit",
} as CSSProperties;

function Tree({ files, onSelect }: { files: ChangedFile[]; onSelect: (path: string) => void }) {
  const { model } = useFileTree({
    paths: files.map((f) => f.name),
    initialExpansion: "open",
    flattenEmptyDirectories: true,
    density: "compact",
    gitStatus: files.map((f) => ({ path: f.name, status: gitStatus[f.type] })),
    onSelectionChange: (paths) => {
      const path = paths.at(-1);
      if (path && files.some((f) => f.name === path)) onSelect(path);
    },
  });
  return (
    <FileTree model={model} className="block min-h-0 border-r border-line" style={treeColors} />
  );
}

const fileStatus: Record<FileDiffMetadata["type"], [string, string, ReactNode]> = {
  change: [
    "Modified",
    "text-blue-500",
    <circle key="d" cx="8" cy="8" r="2.5" fill="currentColor" />,
  ],
  new: ["Added", "text-ok", <path key="d" d="M8 5v6M5 8h6" />],
  deleted: ["Deleted", "text-bad", <path key="d" d="M5 8h6" />],
  "rename-pure": ["Renamed", "text-warn", <path key="d" d="M5 8h6M9 6l2 2-2 2" />],
  "rename-changed": ["Renamed", "text-warn", <path key="d" d="M5 8h6M9 6l2 2-2 2" />],
};

function FileStatus({ type }: { type: FileDiffMetadata["type"] }) {
  const [label, color, mark] = fileStatus[type];
  return (
    <svg
      viewBox="0 0 16 16"
      className={`size-4 shrink-0 ${color}`}
      fill="none"
      stroke="currentColor"
      strokeWidth="1.5"
      strokeLinecap="round"
      role="img"
      aria-label={label}
    >
      <title>{label}</title>
      <rect x="1.5" y="1.5" width="13" height="13" rx="3.5" />
      {mark}
    </svg>
  );
}

function Tool({
  label,
  d,
  on = false,
  onClick,
}: {
  label: string;
  d: string;
  on?: boolean;
  onClick: () => void;
}) {
  return (
    <button
      type="button"
      onClick={onClick}
      title={label}
      aria-label={label}
      aria-pressed={on}
      className={`grid size-8 place-items-center rounded-md hover:bg-sunken hover:text-fg ${on ? "bg-sunken text-fg" : "text-muted"}`}
    >
      <Icon d={d} />
    </button>
  );
}

function Icon({ d }: { d: string }) {
  return (
    <svg
      viewBox="0 0 16 16"
      className="size-4"
      fill="none"
      stroke="currentColor"
      strokeWidth="1.5"
      strokeLinecap="round"
      strokeLinejoin="round"
      aria-hidden
    >
      <path d={d} />
    </svg>
  );
}
