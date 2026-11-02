// PROTOTYPE: fixture for the Repos page layout options. Wipe with the rest of src/prototype.
import { useState } from "react";
import type { RepoView } from "../../api/RepoView";

export type Skill = "current" | "outdated" | "missing";
/** A repo as the page would see it once each row's skill query lands. `skill` is null without a
 * config file: the skill check only runs on a repo that has one. */
export type Repo = RepoView & { skill: Skill | null; pr?: number };

const on = (repo: string, priv: boolean, skill: Skill): Repo => ({
  repo: `agent-labs-dev/${repo}`,
  private: priv,
  on: true,
  enabled: null,
  has_config: true,
  config_error: null,
  skill,
});
const off = (repo: string, priv: boolean): Repo => ({
  repo: `agent-labs-dev/${repo}`,
  private: priv,
  on: false,
  enabled: null,
  has_config: false,
  config_error: null,
  skill: null,
});

/** The workspace in the screenshot: agent-labs-dev's real repos and their real states. */
export const FIXTURE: Repo[] = [
  on("bugpatrol", false, "outdated"),
  on("fastbrowse", false, "outdated"),
  on("fastbrowse-hosted", true, "missing"),
  on("fastbrowse-site", true, "missing"),
  on("mergeloop", false, "current"),
  on("mergepatrol-stack-lab", true, "missing"),
  on("nebula", true, "outdated"),
  on("nebula-desktop", true, "outdated"),
  on("nebula-docs", true, "outdated"),
  { ...off("nebula-metabase", true), on: true, enabled: true },
  on("nebula-mobile", true, "outdated"),
  on("nebula-web", true, "outdated"),
  // Not in the screenshot: a paused repo, so each layout has to show it.
  {
    ...on("parallax", true, "outdated"),
    config_error: "merge_method: unknown variant `rebase-squash`",
  },
  on("sloppatrol", false, "outdated"),
  on("speedpatrol", false, "outdated"),
  off("electron-mcp", false),
  off("homebrew-nebula", false),
  off("jev-ultrafast", true),
  off("mergepatrol-fork-lab", false),
  off("nebula-ai-skill", false),
  off("nebula-blog", true),
  off("paas", true),
  off("pier", true),
  off("pier-scratch-gh-behavior", true),
  off("pier-scratch-protected", true),
  off("tally", true),
  off("visualizer", true),
  off("warden", true),
  // Not in the screenshot: a repo with a config file turned off here.
  { ...on("warden-trust", false, "current"), on: false, enabled: false },
  off("warden-web", true),
  off("zero-python", false),
];

let nextPr = 412;

/** The page's state and actions, against memory: every variant gets the same store. */
export function useRepos() {
  const [repos, setRepos] = useState(FIXTURE);
  const patch = (names: string[], f: (r: Repo) => Partial<Repo>) =>
    setRepos((rs) => rs.map((r) => (names.includes(r.repo) ? { ...r, ...f(r) } : r)));
  return {
    repos,
    setOn: (names: string[], value: boolean) => patch(names, () => ({ on: value, enabled: value })),
    follow: (name: string) =>
      patch([name], (r) => ({ enabled: null, on: r.has_config && !r.config_error })),
    // Opening a PR, as the real buttons do: the row then links it.
    // A config file that doesn't parse needs a person, not a PR: those rows link the file.
    openPr: (names: string[]) => patch(names, (r) => (r.on && r.config_error ? {} : { pr: nextPr++ })),
  };
}
export type Store = ReturnType<typeof useRepos>;

export const short = (repo: string) => repo.split("/")[1];

/** What a repo needs from you, worst first, or null when it is fine. */
export function problem(r: Repo): { label: string; action: string; tone: "bad" | "warn" } | null {
  if (r.on && r.config_error) return { label: "Config doesn't parse", action: "Open file", tone: "bad" };
  if (r.on && !r.has_config) return { label: "No config file", action: "Set up", tone: "warn" };
  if (r.skill === "missing") return { label: "No skill", action: "Add skill", tone: "warn" };
  if (r.skill === "outdated") return { label: "Skill outdated", action: "Update skill", tone: "warn" };
  return null;
}
