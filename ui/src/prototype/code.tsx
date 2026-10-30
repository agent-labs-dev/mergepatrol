/*
 * PROTOTYPE, throw away. The code primitives the PR detail variants share: the changed files as
 * a tree (@pierre/trees) and each file's diff (@pierre/diffs). Its own chunk, like `FixDiff`.
 */
import { type FileDiffMetadata, parsePatchFiles } from "@pierre/diffs";
import { FileDiff } from "@pierre/diffs/react";
import type { GitStatus } from "@pierre/trees";
import { FileTree, useFileTree } from "@pierre/trees/react";
import { type CSSProperties, useMemo, useSyncExternalStore } from "react";
import patch from "./fixture.patch?raw";

export type ChangedFile = FileDiffMetadata & { additions: number; deletions: number };

export function useChangedFiles(): ChangedFile[] {
  return useMemo(
    () =>
      parsePatchFiles(patch)
        .flatMap((p) => p.files)
        .map((f) => ({
          ...f,
          additions: f.hunks.reduce((n, h) => n + h.additionLines, 0),
          deletions: f.hunks.reduce((n, h) => n + h.deletionLines, 0),
        })),
    [],
  );
}

const statusOf: Record<FileDiffMetadata["type"], GitStatus> = {
  change: "modified",
  new: "added",
  deleted: "deleted",
  "rename-pure": "renamed",
  "rename-changed": "renamed",
};

/** The tree's colors, from the dashboard's own. */
const treeStyle = {
  "--trees-bg-override": "transparent",
  "--trees-fg-override": "var(--fg)",
  "--trees-fg-muted-override": "var(--muted)",
  "--trees-selected-bg-override": "var(--sunken)",
  "--trees-selected-fg-override": "var(--fg)",
  "--trees-border-color-override": "var(--line)",
  "--trees-bg-muted-override": "var(--page)",
  "--trees-font-size-override": "12px",
  "--trees-search-bg-override": "var(--surface)",
  "--trees-search-fg-override": "var(--fg)",
  "--trees-input-bg-override": "var(--surface)",
  colorScheme: "inherit",
  "--trees-git-added-color-override": "var(--ok)",
  "--trees-git-deleted-color-override": "var(--bad)",
  "--trees-git-modified-color-override": "var(--warn)",
} as CSSProperties;

export function ChangedFilesTree({
  files,
  onSelect,
  search = false,
  stats = true,
  className,
  style,
}: {
  files: ChangedFile[];
  onSelect: (path: string) => void;
  search?: boolean;
  /** `+n −n` at the end of each file's row. */
  stats?: boolean;
  className?: string;
  style?: CSSProperties;
}) {
  const byPath = useMemo(() => new Map(files.map((f) => [f.name, f])), [files]);
  const { model } = useFileTree({
    paths: files.map((f) => f.name),
    initialExpansion: "open",
    flattenEmptyDirectories: true,
    search,
    density: "compact",
    gitStatus: files.map((f) => ({ path: f.name, status: statusOf[f.type] })),
    initialSelectedPaths: files[0] ? [files[0].name] : [],
    onSelectionChange: (paths) => {
      const path = paths.at(-1);
      if (path && byPath.has(path)) onSelect(path);
    },
    renderRowDecoration: stats
      ? ({ item }) => {
          const f = byPath.get(item.path);
          if (!f) return null;
          return {
            text: `+${f.additions} −${f.deletions}`,
            parts: [
              { text: `+${f.additions}`, color: "var(--ok)" },
              { text: "\u2002" },
              { text: `−${f.deletions}`, color: "var(--bad)" },
            ],
          };
        }
      : undefined,
  });
  return <FileTree model={model} className={className} style={{ ...treeStyle, ...style }} />;
}

export function Diff({
  file,
  split = false,
  header = true,
  id,
}: {
  file: ChangedFile;
  split?: boolean;
  header?: boolean;
  id?: string;
}) {
  const dark = useDark();
  return (
    <div id={id} className="scroll-mt-14">
      <FileDiff
        fileDiff={file}
        className="block overflow-hidden rounded-md border border-line"
        style={{ "--diffs-font-size": "12px" } as CSSProperties}
        options={{
          diffStyle: split ? "split" : "unified",
          themeType: dark ? "dark" : "light",
          overflow: split ? "scroll" : "wrap",
          hunkSeparators: "line-info",
          lineDiffType: "word",
          disableFileHeader: !header,
        }}
      />
    </div>
  );
}

export const fileAnchor = (path: string) => `proto-file-${path.replace(/[^a-z0-9]+/gi, "-")}`;

const useDark = () =>
  useSyncExternalStore(
    (onChange) => {
      const o = new MutationObserver(onChange);
      o.observe(document.documentElement, { attributes: true, attributeFilter: ["class"] });
      return () => o.disconnect();
    },
    () => document.documentElement.classList.contains("dark"),
  );
