/*
 * PROTOTYPE, throw away. Three variants of the Queue page's detail pane for an open PR that is
 * not queued yet, switchable via `?variant=A|B|C` on the existing `/$account/queue` route.
 * The PR fields are real (the open PRs endpoint); body, timeline, check runs and the patch are
 * stubs from `fixtures.ts`. Queue and Priority call the real actions.
 */
import { type ReactNode, useEffect, useMemo, useState } from "react";
import type { OpenPullView } from "../api/OpenPullView";
import { Avatar, primary, secondary, timeAgo } from "../ui";
import { type ChangedFile, ChangedFilesTree, Diff, fileAnchor, useChangedFiles } from "./code";
import {
  type CheckRun,
  type Comment,
  extrasFor,
  type PrExtras,
  type TimelineItem,
} from "./fixtures";
import type { Variant } from "./variants";

export type PrDetailProps = {
  pull: OpenPullView;
  repo: string;
  trunk: string;
  onQueue: (priority: boolean) => void;
  busy: boolean;
  onBack: () => void;
};

export default function PrDetailPrototype({
  variant,
  ...props
}: PrDetailProps & { variant: Variant }) {
  if (variant === "B") return <VariantB {...props} />;
  if (variant === "C") return <VariantC {...props} />;
  return <VariantA {...props} />;
}

/* ---------- Bits the variants share: data, not layout ---------- */

const useExtras = (n: number) => useMemo(() => extrasFor(n), [n]);
const gh = (repo: string, n: number) => `https://github.com/${repo}/pull/${n}`;
const avatar = (login: string) => `https://github.com/${login}.png?size=40`;

function Stat({ additions, deletions }: { additions: number; deletions: number }) {
  return (
    <span className="font-mono tabular-nums">
      <span className="text-ok">+{additions.toLocaleString()}</span>{" "}
      <span className="text-bad">−{deletions.toLocaleString()}</span>
    </span>
  );
}

const runIcon: Record<CheckRun["status"], [string, string]> = {
  passed: ["✓", "text-ok"],
  failed: ["✕", "text-bad"],
  pending: ["●", "text-warn"],
  skipped: ["–", "text-faint"],
};

function CheckList({ runs }: { runs: CheckRun[] }) {
  // Failing first: that's what someone opening the pane is looking for.
  const order = { failed: 0, pending: 1, passed: 2, skipped: 3 };
  return (
    <ul className="divide-y divide-line-soft">
      {[...runs]
        .sort((a, b) => order[a.status] - order[b.status])
        .map((r) => (
          <li key={r.name} className="flex items-center gap-2 py-1.5 text-sm">
            <span className={`w-3 text-center ${runIcon[r.status][1]}`}>
              {runIcon[r.status][0]}
            </span>
            <span className="min-w-0 flex-1 truncate font-mono text-xs">{r.name}</span>
            <span className="text-xs text-faint tabular-nums">{r.took}</span>
          </li>
        ))}
    </ul>
  );
}

const passing = (runs: CheckRun[]) => runs.filter((r) => r.status === "passed").length;

/** Markdown enough for a PR body: headings, lists, task lists, fenced code. */
function Body({ text }: { text: string }) {
  const blocks = text.split(/\n{2,}(?=[^\n])/);
  return (
    <div className="space-y-3 text-sm leading-relaxed text-fg-2">
      {blocks.map((b, i) => {
        const key = `${i}`;
        if (b.startsWith("```"))
          return (
            <pre
              key={key}
              className="overflow-x-auto rounded-md bg-sunken p-3 font-mono text-xs text-fg"
            >
              {b.replace(/^```\w*\n?|```$/g, "").trim()}
            </pre>
          );
        if (b.startsWith("## "))
          return (
            <h4 key={key} className="pt-2 text-sm font-semibold text-fg">
              {b.slice(3)}
            </h4>
          );
        if (/^- /.test(b))
          return (
            <ul key={key} className="space-y-1">
              {b.split("\n").map((l) => {
                const task = l.match(/^- \[( |x)\] (.*)/);
                return (
                  <li key={l} className="flex gap-2">
                    {task ? (
                      <input type="checkbox" readOnly checked={task[1] === "x"} className="mt-1" />
                    ) : (
                      <span className="text-faint">•</span>
                    )}
                    <span>{code(task ? task[2] : l.slice(2))}</span>
                  </li>
                );
              })}
            </ul>
          );
        return <p key={key}>{code(b)}</p>;
      })}
    </div>
  );
}
const code = (s: string) =>
  s.split(/(`[^`]+`)/).map((p, i) =>
    p.startsWith("`") ? (
      // biome-ignore lint/suspicious/noArrayIndexKey: static text
      <code key={i} className="rounded bg-sunken px-1 font-mono text-xs text-fg">
        {p.slice(1, -1)}
      </code>
    ) : (
      p
    ),
  );

const reviewText = { approved: "approved", changes: "requested changes", commented: "reviewed" };

function TimelineEntry({ item: t, dense }: { item: TimelineItem; dense?: boolean }) {
  const what: ReactNode =
    t.kind === "commit" ? (
      <>
        pushed <code className="font-mono text-xs text-fg-2">{t.sha}</code>{" "}
        <span className="text-fg-2">{t.message}</span>
      </>
    ) : t.kind === "force-push" ? (
      <>
        force-pushed <code className="font-mono text-xs">{t.from}</code> →{" "}
        <code className="font-mono text-xs">{t.to}</code>
      </>
    ) : t.kind === "review" ? (
      <span
        className={t.state === "approved" ? "text-ok" : t.state === "changes" ? "text-bad" : ""}
      >
        {reviewText[t.state]}
      </span>
    ) : t.kind === "requested" ? (
      <>requested a review from {t.reviewer}</>
    ) : t.kind === "label" ? (
      <>
        {t.added ? "added" : "removed"}{" "}
        <span className="rounded-full bg-sunken px-1.5 py-0.5 text-xs text-fg-2">{t.label}</span>
      </>
    ) : (
      <>commented</>
    );
  const body = (t.kind === "comment" || t.kind === "review") && t.body;
  return (
    <li className="flex gap-3">
      <Avatar src={avatar(t.login)} alt={t.login} size={dense ? "size-4" : "size-5"} />
      <div className="min-w-0 flex-1">
        <p className="text-sm text-muted">
          <span className="font-medium text-fg">{t.login}</span> {what}{" "}
          <span className="whitespace-nowrap text-faint">· {timeAgo(t.at)}</span>
        </p>
        {body && !dense && (
          <p className="mt-1.5 rounded-md border border-line bg-surface px-3 py-2 text-sm text-fg-2">
            {body}
          </p>
        )}
      </div>
    </li>
  );
}

function Timeline({ items, dense }: { items: TimelineItem[]; dense?: boolean }) {
  return (
    <ol className={dense ? "space-y-2.5" : "space-y-4"}>
      {items.map((t) => (
        <TimelineEntry key={`${t.kind}${t.at}`} item={t} dense={dense} />
      ))}
    </ol>
  );
}

const decisionText = {
  approved: ["Approved", "text-ok"],
  review_required: ["Review required", "text-warn"],
  changes_requested: ["Changes requested", "text-bad"],
} as const;

/** What would hold the PR once queued, as the queue sees it. */
function gates(p: OpenPullView, x: PrExtras) {
  const failed = x.checkRuns.filter((r) => r.status === "failed");
  const running = x.checkRuns.filter((r) => r.status === "pending");
  return [
    {
      label: "Ready for review",
      ok: !p.draft,
      why: "A draft: the queue holds it until it's ready.",
    },
    {
      label: "Reviews",
      ok: p.review_decision !== "changes_requested" && p.review_decision !== "review_required",
      why:
        p.review_decision === "changes_requested"
          ? "Changes requested: held until approved."
          : "Needs an approving review.",
    },
    {
      label: "Checks",
      ok: failed.length === 0,
      warn: running.length > 0,
      why: failed.length
        ? `${failed.length} failing: ${failed.map((f) => f.name).join(", ")}. The fix agent will try once queued.`
        : `${running.length} still running.`,
    },
    {
      label: "Mergeable",
      ok: x.mergeable !== "conflicts",
      warn: x.mergeable === "behind",
      why:
        x.mergeable === "conflicts"
          ? "Conflicts with the trunk. The resolver will try once queued."
          : `Behind the trunk. The queue rebases it.`,
    },
  ];
}

function QueueButtons({
  onQueue,
  busy,
  compact,
}: Pick<PrDetailProps, "onQueue" | "busy"> & { compact?: boolean }) {
  return (
    <span className="flex shrink-0 gap-2">
      <button
        type="button"
        disabled={busy}
        onClick={() => onQueue(false)}
        className={`${primary} ${compact ? "h-8 px-3" : ""}`}
      >
        Queue
      </button>
      <button
        type="button"
        disabled={busy}
        onClick={() => onQueue(true)}
        className={`${secondary} ${compact ? "h-8 px-3" : ""} text-hot!`}
        title="Queue at the front"
      >
        ↑ Priority
      </button>
    </span>
  );
}

function Back({ repo, onBack }: { repo: string; onBack: () => void }) {
  return (
    <button type="button" onClick={onBack} className="mb-3 text-muted hover:text-fg lg:hidden">
      ← {repo.split("/")[1]}
    </button>
  );
}

/* ---------- A: header + Summary / Timeline / Code tabs, like T3 Code's linked PR ---------- */

function VariantA({ pull: p, repo, trunk, onQueue, busy, onBack }: PrDetailProps) {
  const x = useExtras(p.number);
  const files = useChangedFiles();
  const [tab, setTab] = useState<"summary" | "timeline" | "code">("summary");
  const total = x.checkRuns.length;
  const ok = passing(x.checkRuns);
  return (
    <div className="flex h-full flex-col">
      <div className="border-b border-line px-4 pt-4 sm:px-6">
        <Back repo={repo} onBack={onBack} />
        <div className="flex items-center justify-between gap-3">
          <a
            href={gh(repo, p.number)}
            target="_blank"
            rel="noreferrer"
            className="truncate text-muted hover:text-fg"
          >
            {repo} <span className="text-ok">#{p.number}</span> ↗
          </a>
          <QueueButtons onQueue={onQueue} busy={busy} compact />
        </div>
        <h2 className="mt-2 text-lg font-semibold tracking-tight">{p.title}</h2>
        <div className="mt-1.5 flex flex-wrap items-center justify-between gap-2 text-sm text-muted">
          <span className="flex items-center gap-1.5">
            {p.author && <Avatar src={p.author.avatar_url} alt={p.author.login} size="size-4" />}
            <span className="text-fg-2">{p.author?.login}</span> · opened {timeAgo(p.created_at)}
          </span>
          <code className="font-mono text-xs">gh pr checkout {p.number}</code>
        </div>
        <div className="mt-3 flex items-center justify-between gap-3 font-mono text-xs">
          <span className="truncate">
            <span className={x.mergeable === "clean" ? "text-fg-2" : "text-warn"}>
              {trunk}
              {x.mergeable !== "clean" && " ⚠"}
            </span>{" "}
            <span className="text-faint">←</span> <span className="text-muted">{p.head_ref}</span>
          </span>
          <span className="shrink-0 text-muted">
            {p.changed_files} files <Stat additions={p.additions} deletions={p.deletions} />
          </span>
        </div>
        <div className="mt-3 flex items-center justify-between">
          <div className="flex gap-1 rounded-lg bg-sunken p-1">
            {(["summary", "timeline", "code"] as const).map((t) => (
              <button
                key={t}
                type="button"
                onClick={() => setTab(t)}
                className={`rounded-md px-3 py-1 text-sm capitalize ${
                  tab === t
                    ? "bg-surface font-medium shadow-xs ring-1 ring-line"
                    : "text-muted hover:text-fg"
                }`}
              >
                {t}
              </button>
            ))}
          </div>
          <button
            type="button"
            onClick={() => {
              setTab("summary");
              requestAnimationFrame(() =>
                document.getElementById("proto-checks")?.scrollIntoView({ behavior: "smooth" }),
              );
            }}
            className="text-sm text-muted hover:text-fg"
          >
            {x.checkRuns.some((r) => r.status === "failed") ? (
              <span className="text-bad">✕</span>
            ) : ok === total ? (
              <span className="text-ok">✓</span>
            ) : (
              <span className="text-warn">●</span>
            )}{" "}
            {ok} of {total} passing
          </button>
        </div>
        <div className="h-3" />
      </div>
      {tab === "summary" && (
        <div className="min-h-0 flex-1 overflow-y-auto px-4 py-5 sm:px-6">
          <dl className="grid grid-cols-[100px_1fr] items-center gap-x-4 gap-y-3 text-sm">
            <dt className="text-muted">Reviewers</dt>
            <dd className="flex items-center gap-1.5">
              {p.reviews.map((r) => (
                <span key={r.reviewer.login} className="relative">
                  <Avatar src={r.reviewer.avatar_url} alt={r.reviewer.login} />
                  <span
                    className={`absolute -right-1 -bottom-1 text-[10px] ${r.approved ? "text-ok" : "text-bad"}`}
                  >
                    {r.approved ? "✓" : "✕"}
                  </span>
                </span>
              ))}
              {p.requested.map((r) => (
                <span key={r.login} className="opacity-50">
                  <Avatar src={r.avatar_url} alt={`${r.login} (pending)`} />
                </span>
              ))}
              {p.review_decision && (
                <span className={`ml-2 text-xs ${decisionText[p.review_decision][1]}`}>
                  {decisionText[p.review_decision][0]}
                </span>
              )}
            </dd>
            <dt className="text-muted">Labels</dt>
            <dd>
              {x.labels.length ? (
                x.labels.map((l) => (
                  <span key={l} className="rounded-full bg-sunken px-2 py-0.5 text-xs text-fg-2">
                    {l}
                  </span>
                ))
              ) : (
                <span className="text-faint">None</span>
              )}
            </dd>
          </dl>
          <Section title="Description">
            <Body text={x.body} />
          </Section>
          <Section id="proto-checks" title="Checks">
            <CheckRows runs={x.checkRuns} />
          </Section>
          <Comments comments={x.comments} />
        </div>
      )}
      {tab === "timeline" && (
        <div className="min-h-0 flex-1 overflow-y-auto px-4 py-5 sm:px-6">
          <Timeline items={x.timeline} />
        </div>
      )}
      {tab === "code" && <CodeTab files={files} commits={x.timeline} />}
    </div>
  );
}

/** A collapsible section of A's Summary, open by default. */
function Section({
  id,
  title,
  actions,
  children,
}: {
  id?: string;
  title: ReactNode;
  actions?: ReactNode;
  children: ReactNode;
}) {
  const [open, setOpen] = useState(true);
  return (
    <section id={id} className="mt-8 scroll-mt-4">
      <div className="mb-3 flex items-center justify-between gap-3">
        <button
          type="button"
          onClick={() => setOpen((o) => !o)}
          className="flex items-center gap-1.5 text-muted hover:text-fg"
        >
          {title}
          <span className={`transition-transform ${open ? "" : "-rotate-90"}`}>
            <Icon d="M4 6l4 4 4-4" />
          </span>
        </button>
        {open && actions}
      </div>
      {open && children}
    </section>
  );
}

const runStatus: Record<CheckRun["status"], [string, string, string]> = {
  passed: ["Passed", "text-ok", "M5.5 8.2l1.8 1.8 3.2-3.6"],
  failed: ["Failed", "text-bad", "M6 6l4 4M10 6l-4 4"],
  pending: ["Running", "text-warn", "M8 5v3l2 1.5"],
  skipped: ["Skipped", "text-faint", "M5.5 8h5"],
};

function CheckRows({ runs }: { runs: CheckRun[] }) {
  const order = { failed: 0, pending: 1, passed: 2, skipped: 3 };
  return (
    <ul className="space-y-1">
      {[...runs]
        .sort((a, b) => order[a.status] - order[b.status])
        .map((r) => {
          const [label, color, d] = runStatus[r.status];
          return (
            <li
              key={r.name}
              className="flex items-center gap-3 rounded-md px-2 py-1.5 hover:bg-sunken"
            >
              <svg viewBox="0 0 16 16" className={`size-4 shrink-0 ${color}`} aria-hidden>
                <circle
                  cx="8"
                  cy="8"
                  r="6.25"
                  fill="none"
                  stroke="currentColor"
                  strokeWidth="1.25"
                />
                <path
                  d={d}
                  fill="none"
                  stroke="currentColor"
                  strokeWidth="1.25"
                  strokeLinecap="round"
                  strokeLinejoin="round"
                />
              </svg>
              <span className="min-w-0 flex-1 truncate">{r.name}</span>
              <span className="text-xs text-faint tabular-nums">{r.took}</span>
              <span
                className={`w-16 text-right text-sm ${r.status === "passed" ? "text-muted" : color}`}
              >
                {label}
              </span>
            </li>
          );
        })}
    </ul>
  );
}

const commentKinds = { all: "All", comment: "Comments", review: "Reviews", bot: "Bots" } as const;

function Comments({ comments }: { comments: Comment[] }) {
  const [kind, setKind] = useState<keyof typeof commentKinds>("all");
  const [newest, setNewest] = useState(true);
  const shown = comments
    .filter((c) => kind === "all" || c.kind === kind)
    .sort((a, b) => (newest ? b.at.localeCompare(a.at) : a.at.localeCompare(b.at)));
  const quiet = "flex items-center gap-1.5 text-muted hover:text-fg";
  return (
    <Section
      title={`Comments (${comments.length})`}
      actions={
        comments.length > 0 && (
          <span className="flex items-center gap-4 text-sm">
            <label className={quiet}>
              <Icon d="M3 4.5h10M5 8h6M7 11.5h2" />
              <select
                value={kind}
                onChange={(e) => setKind(e.target.value as keyof typeof commentKinds)}
                // The global select chevron is unlayered, so utilities can't drop it.
                style={{ backgroundImage: "none", paddingRight: 0 }}
                className="h-auto w-auto border-0 bg-transparent p-0 text-sm text-inherit shadow-none"
              >
                {Object.entries(commentKinds).map(([k, v]) => (
                  <option key={k} value={k}>
                    {k === "all" ? "Filter" : v}
                  </option>
                ))}
              </select>
            </label>
            <button type="button" onClick={() => setNewest((n) => !n)} className={quiet}>
              <Icon d="M5 3v10M3 11l2 2 2-2M11 13V3M9 5l2-2 2 2" />
              {newest ? "Newest first" : "Oldest first"}
            </button>
          </span>
        )
      }
    >
      {comments.length === 0 ? (
        <p className="text-muted">No comments yet.</p>
      ) : shown.length === 0 ? (
        <p className="text-muted">No {commentKinds[kind].toLowerCase()}.</p>
      ) : (
        <ol className="space-y-3">
          {shown.map((c) => (
            <li key={c.at} className="rounded-lg border border-line bg-surface">
              <p className="flex items-center gap-2 border-b border-line-soft px-3 py-2 text-sm">
                <Avatar src={avatar(c.login)} alt={c.login} size="size-5" />
                <span className="font-medium">{c.login}</span>
                {c.kind !== "comment" && (
                  <span className="rounded-full bg-sunken px-1.5 py-0.5 text-xs text-muted">
                    {c.kind === "bot" ? "bot" : "review"}
                  </span>
                )}
                <span className="ml-auto text-xs text-faint">{timeAgo(c.at)}</span>
              </p>
              <p className="px-3 py-2.5 text-sm text-fg-2">{c.body}</p>
            </li>
          ))}
        </ol>
      )}
    </Section>
  );
}

/** A's Code tab: every changed file as a row that opens to its diff, GitHub's "Files changed"
 * in a pane. Viewed closes a file and counts it. The tree toggle adds a @pierre/trees navigator. */
function CodeTab({ files, commits }: { files: ChangedFile[]; commits: TimelineItem[] }) {
  const [open, setOpen] = useState(new Set<string>());
  const [viewed, setViewed] = useState(new Set<string>());
  const [split, setSplit] = useState(false);
  const [tree, setTree] = useState(false);
  const [commit, setCommit] = useState("all");
  const toggle = (set: Set<string>, path: string, on = !set.has(path)) => {
    const next = new Set(set);
    if (on) next.add(path);
    else next.delete(path);
    return next;
  };
  const allOpen = open.size === files.length;
  const reveal = (path: string) => {
    setOpen((o) => toggle(o, path, true));
    requestAnimationFrame(() =>
      document.getElementById(fileAnchor(path))?.scrollIntoView({ block: "start" }),
    );
  };
  const tool = (on = false) =>
    `grid size-8 place-items-center rounded-md hover:bg-sunken hover:text-fg ${on ? "bg-sunken text-fg" : "text-muted"}`;
  return (
    <div className="flex min-h-0 flex-1 flex-col">
      <div className="flex items-center gap-3 border-b border-line px-4 py-2 sm:px-6">
        <select
          value={commit}
          onChange={(e) => setCommit(e.target.value)}
          className="h-8 w-auto max-w-40 shrink-0 truncate rounded-md border-0 bg-sunken pl-3 text-sm"
        >
          <option value="all">All commits</option>
          {commits.flatMap((c) =>
            c.kind === "commit"
              ? [
                  <option key={c.sha} value={c.sha}>
                    {c.sha} {c.message}
                  </option>,
                ]
              : [],
          )}
        </select>
        <span className="truncate text-sm whitespace-nowrap text-muted tabular-nums">
          {files.length} files · {viewed.size} / {files.length} viewed
        </span>
        <span className="ml-auto flex items-center gap-0.5">
          <button
            type="button"
            onClick={() => setSplit((v) => !v)}
            className={tool(split)}
            title={split ? "Unified diff" : "Split diff"}
          >
            <Icon d="M2.5 4.5h7M12.5 4.5h1M2.5 11.5h1M6.5 11.5h7M11 3v3M5 10v3" />
          </button>
          <button
            type="button"
            onClick={() => setOpen(allOpen ? new Set() : new Set(files.map((f) => f.name)))}
            className={tool()}
            title={allOpen ? "Collapse all" : "Expand all"}
          >
            <Icon d={allOpen ? "M5 3l3 3 3-3M5 13l3-3 3 3" : "M5 6l3-3 3 3M5 10l3 3 3-3"} />
          </button>
          <button
            type="button"
            onClick={() => setTree((t) => !t)}
            className={tool(tree)}
            title="File tree"
          >
            <Icon d="M3.5 2.5v9h4M3.5 6.5h4M9.5 4.5h4v4h-4zM9.5 9.5h4v4h-4z" />
          </button>
        </span>
      </div>
      <div
        className={`min-h-0 flex-1 ${tree ? "grid grid-cols-[minmax(180px,35%)_1fr]" : "flex flex-col"}`}
      >
        {tree && (
          <ChangedFilesTree
            files={files}
            onSelect={reveal}
            stats={false}
            className="block min-h-0 border-r border-line"
          />
        )}
        <ul className="min-h-0 overflow-y-auto">
          {files.map((f) => {
            const isOpen = open.has(f.name);
            const seen = viewed.has(f.name);
            return (
              <li key={f.name} id={fileAnchor(f.name)} className="border-b border-line-soft">
                <div
                  className={`sticky top-0 z-10 flex items-center gap-3 bg-surface px-4 py-2.5 text-sm sm:px-6 ${seen ? "opacity-60" : ""}`}
                >
                  <button
                    type="button"
                    onClick={() => setOpen((o) => toggle(o, f.name))}
                    className="flex min-w-0 flex-1 items-center gap-3 text-left"
                  >
                    <span
                      className={`text-faint transition-transform ${isOpen ? "rotate-90" : ""}`}
                    >
                      <Icon d="M6 4l4 4-4 4" />
                    </span>
                    <FileStatus type={f.type} />
                    <span className="truncate" title={f.name}>
                      {f.prevName ? `${f.prevName} → ` : ""}
                      {f.name}
                    </span>
                  </button>
                  <Stat additions={f.additions} deletions={f.deletions} />
                  <label className="flex cursor-pointer items-center gap-2 text-muted">
                    <input
                      type="checkbox"
                      checked={seen}
                      onChange={(e) => {
                        setViewed((v) => toggle(v, f.name, e.target.checked));
                        // GitHub's way: marking a file viewed folds it away.
                        if (e.target.checked) setOpen((o) => toggle(o, f.name, false));
                      }}
                      className="size-4 rounded accent-fg"
                    />
                    Viewed
                  </label>
                </div>
                {isOpen && (
                  <div className="px-4 pb-4 sm:px-6">
                    <Diff file={f} split={split} header={false} />
                  </div>
                )}
              </li>
            );
          })}
        </ul>
      </div>
    </div>
  );
}

const fileStatus: Record<ChangedFile["type"], [string, string, string]> = {
  change: ["Modified", "text-blue-500", "M8 5.5a2.5 2.5 0 1 0 0 5 2.5 2.5 0 0 0 0-5Z"],
  new: ["Added", "text-ok", "M8 5v6M5 8h6"],
  deleted: ["Deleted", "text-bad", "M5 8h6"],
  "rename-pure": ["Renamed", "text-warn", "M5 8h6M9 6l2 2-2 2"],
  "rename-changed": ["Renamed", "text-warn", "M5 8h6M9 6l2 2-2 2"],
};

function FileStatus({ type }: { type: ChangedFile["type"] }) {
  const [label, color, d] = fileStatus[type];
  return (
    <svg viewBox="0 0 16 16" className={`size-4 shrink-0 ${color}`} aria-label={label}>
      <rect
        x="1.5"
        y="1.5"
        width="13"
        height="13"
        rx="3.5"
        fill="none"
        stroke="currentColor"
        strokeWidth="1.5"
      />
      <path
        d={d}
        fill={type === "change" ? "currentColor" : "none"}
        stroke="currentColor"
        strokeWidth="1.5"
        strokeLinecap="round"
      />
    </svg>
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

/* ---------- B: what stands between this PR and the trunk, then everything in one scroll ---------- */

function VariantB({ pull: p, repo, trunk, onQueue, busy, onBack }: PrDetailProps) {
  const x = useExtras(p.number);
  const files = useChangedFiles();
  const g = gates(p, x);
  const blocked = g.filter((c) => !c.ok);
  const [descOpen, setDescOpen] = useState(false);
  const jump = (path: string) =>
    document
      .getElementById(fileAnchor(path))
      ?.scrollIntoView({ behavior: "smooth", block: "start" });
  return (
    <div className="p-4 sm:p-6">
      <Back repo={repo} onBack={onBack} />
      <p className="text-sm text-muted">
        {repo} #{p.number} · {p.author?.login} · {timeAgo(p.created_at)}
      </p>
      <h2 className="mt-1 text-lg font-semibold tracking-tight">{p.title}</h2>

      <section
        className={`mt-5 rounded-xl border p-4 ${
          blocked.length
            ? "border-amber-300 bg-amber-50/60 dark:border-amber-400/30 dark:bg-amber-400/5"
            : "border-emerald-300 bg-emerald-50/60 dark:border-emerald-400/30 dark:bg-emerald-400/5"
        }`}
      >
        <div className="flex items-start justify-between gap-3">
          <div>
            <p className="font-medium">
              {blocked.length
                ? `${blocked.length} thing${blocked.length > 1 ? "s" : ""} would hold it`
                : "Ready to queue"}
            </p>
            <p className="mt-0.5 text-sm text-muted">
              Into <span className="font-mono text-xs">{trunk}</span> from{" "}
              <span className="font-mono text-xs">{p.head_ref}</span>
            </p>
          </div>
          <QueueButtons onQueue={onQueue} busy={busy} />
        </div>
        <ul className="mt-4 space-y-2">
          {g.map((c) => (
            <li key={c.label} className="flex gap-2.5 text-sm">
              <span
                className={`mt-px w-4 text-center ${c.ok ? (c.warn ? "text-warn" : "text-ok") : "text-bad"}`}
              >
                {c.ok ? (c.warn ? "●" : "✓") : "✕"}
              </span>
              <span>
                <span className="font-medium">{c.label}</span>
                {(!c.ok || c.warn) && <span className="block text-muted">{c.why}</span>}
              </span>
            </li>
          ))}
        </ul>
      </section>

      <nav className="sticky top-0 z-10 -mx-4 mt-6 flex gap-4 border-b border-line bg-page/95 px-4 py-2 text-sm backdrop-blur sm:-mx-6 sm:px-6">
        {["Description", "Checks", "Files", "Activity"].map((s) => (
          <a key={s} href={`#proto-${s}`} className="text-muted hover:text-fg">
            {s}
          </a>
        ))}
      </nav>

      <section id="proto-Description" className="scroll-mt-12 pt-6">
        <h3 className="mb-3 text-xs font-medium text-muted">Description</h3>
        <div className={descOpen ? "" : "relative max-h-40 overflow-hidden"}>
          <Body text={x.body} />
          {!descOpen && (
            <div className="absolute inset-x-0 bottom-0 h-16 bg-gradient-to-t from-page" />
          )}
        </div>
        <button
          type="button"
          onClick={() => setDescOpen((o) => !o)}
          className="mt-2 text-sm text-muted hover:text-fg"
        >
          {descOpen ? "Less" : "More"}
        </button>
      </section>

      <section id="proto-Checks" className="scroll-mt-12 pt-8">
        <h3 className="mb-2 text-xs font-medium text-muted">
          Checks · {passing(x.checkRuns)} of {x.checkRuns.length} passing
        </h3>
        <CheckList runs={x.checkRuns} />
      </section>

      <section id="proto-Files" className="scroll-mt-12 pt-8">
        <h3 className="mb-2 flex justify-between text-xs font-medium text-muted">
          <span>Files · {files.length}</span>
          <Stat additions={p.additions} deletions={p.deletions} />
        </h3>
        <ChangedFilesTree
          files={files}
          onSelect={jump}
          className="block rounded-md border border-line bg-surface"
          style={{ height: Math.min(files.length * 28 + 90, 260) }}
        />
        <div className="mt-4 space-y-3">
          {files.map((f) => (
            <Diff key={f.name} file={f} id={fileAnchor(f.name)} />
          ))}
        </div>
      </section>

      <section id="proto-Activity" className="scroll-mt-12 pt-8 pb-16">
        <h3 className="mb-3 text-xs font-medium text-muted">Activity</h3>
        <Timeline items={x.timeline} />
      </section>
    </div>
  );
}

/* ---------- C: a summary card; the code opens full width over the queue ---------- */

function VariantC(props: PrDetailProps) {
  const { pull: p, repo, onQueue, busy, onBack } = props;
  const x = useExtras(p.number);
  const [reviewing, setReviewing] = useState(false);
  const ok = passing(x.checkRuns);
  const failed = x.checkRuns.filter((r) => r.status === "failed");
  return (
    <div className="p-4 sm:p-6">
      <Back repo={repo} onBack={onBack} />
      <div className="rounded-xl border border-line bg-surface p-5 shadow-xs">
        <div className="flex items-center gap-2 text-sm text-muted">
          {p.author && <Avatar src={p.author.avatar_url} alt={p.author.login} size="size-5" />}
          <span className="text-fg-2">{p.author?.login}</span>
          <span>· #{p.number}</span>
          <span>· {timeAgo(p.created_at)}</span>
        </div>
        <h2 className="mt-2 text-xl font-semibold tracking-tight">{p.title}</h2>
        <p className="mt-2 line-clamp-3 text-sm text-muted">{x.body.split("\n")[0]}</p>
        <div className="mt-5 grid grid-cols-2 gap-px overflow-hidden rounded-lg border border-line bg-line text-sm sm:grid-cols-4">
          <Tile label="Checks" tone={failed.length ? "bad" : "ok"}>
            {ok}/{x.checkRuns.length}
          </Tile>
          <Tile
            label="Review"
            tone={
              p.review_decision === "approved"
                ? "ok"
                : p.review_decision === "changes_requested"
                  ? "bad"
                  : "warn"
            }
          >
            {p.review_decision ? decisionText[p.review_decision][0] : "None"}
          </Tile>
          <Tile
            label="Trunk"
            tone={x.mergeable === "conflicts" ? "bad" : x.mergeable === "behind" ? "warn" : "ok"}
          >
            {x.mergeable === "clean" ? "Clean" : x.mergeable === "behind" ? "Behind" : "Conflicts"}
          </Tile>
          <Tile label={`${p.changed_files} files`}>
            <Stat additions={p.additions} deletions={p.deletions} />
          </Tile>
        </div>
        {failed.length > 0 && (
          <p className="mt-3 text-sm text-bad">✕ {failed.map((f) => f.name).join(", ")}</p>
        )}
        <div className="mt-5 flex flex-wrap items-center gap-2">
          <QueueButtons onQueue={onQueue} busy={busy} />
          <button
            type="button"
            onClick={() => setReviewing(true)}
            className={`${secondary} ml-auto gap-2`}
          >
            Review changes{" "}
            <kbd className="rounded border border-line px-1 text-[10px] text-faint">R</kbd>
          </button>
        </div>
      </div>
      <h3 className="mt-8 mb-3 text-xs font-medium text-muted">Latest</h3>
      <Timeline items={x.timeline.slice(-4).reverse()} dense />
      <ReviewHotkey onOpen={() => setReviewing(true)} />
      {reviewing && <Review {...props} extras={x} onClose={() => setReviewing(false)} />}
    </div>
  );
}

function Tile({
  label,
  tone,
  children,
}: {
  label: string;
  tone?: "ok" | "bad" | "warn";
  children: ReactNode;
}) {
  const color = tone ? { ok: "text-ok", bad: "text-bad", warn: "text-warn" }[tone] : "";
  return (
    <div className="bg-surface px-3 py-2.5">
      <p className="text-xs text-muted">{label}</p>
      <p className={`mt-0.5 font-medium ${color}`}>{children}</p>
    </div>
  );
}

function ReviewHotkey({ onOpen }: { onOpen: () => void }) {
  useEffect(() => {
    const on = (e: KeyboardEvent) => {
      if (e.key === "r" && !(e.target as HTMLElement).closest("input,textarea,[contenteditable]"))
        onOpen();
    };
    window.addEventListener("keydown", on);
    return () => window.removeEventListener("keydown", on);
  }, [onOpen]);
  return null;
}

function Review({
  pull: p,
  repo,
  trunk,
  onQueue,
  busy,
  extras: x,
  onClose,
}: PrDetailProps & { extras: PrExtras; onClose: () => void }) {
  const files = useChangedFiles();
  const [file, setFile] = useState(files[0]?.name);
  const i = Math.max(
    0,
    files.findIndex((f) => f.name === file),
  );
  const selected: ChangedFile | undefined = files[i];
  useEffect(() => {
    const on = (e: KeyboardEvent) => {
      if (e.key === "Escape") onClose();
      if (e.key === "j") setFile(files[Math.min(i + 1, files.length - 1)]?.name);
      if (e.key === "k") setFile(files[Math.max(i - 1, 0)]?.name);
    };
    window.addEventListener("keydown", on);
    return () => window.removeEventListener("keydown", on);
  }, [files, i, onClose]);
  return (
    <div className="fixed inset-0 z-40 flex flex-col bg-page">
      <div className="flex h-12 shrink-0 items-center gap-3 border-b border-line bg-surface px-4">
        <button
          type="button"
          onClick={onClose}
          className="text-muted hover:text-fg"
          title="Close (Esc)"
        >
          ✕
        </button>
        <span className="min-w-0 truncate font-medium">{p.title}</span>
        <span className="shrink-0 text-sm text-muted">
          {repo} #{p.number} ·{" "}
          <span className="font-mono text-xs">
            {trunk} ← {p.head_ref}
          </span>
        </span>
        <span className="ml-auto">
          <QueueButtons onQueue={onQueue} busy={busy} compact />
        </span>
      </div>
      <div className="grid min-h-0 flex-1 grid-cols-[280px_1fr_300px]">
        <div className="flex min-h-0 flex-col border-r border-line bg-surface">
          <p className="flex justify-between px-3 pt-3 pb-1 text-xs text-muted">
            <span>{files.length} files</span>
            <span>j / k</span>
          </p>
          <ChangedFilesTree
            files={files}
            onSelect={setFile}
            search
            className="block min-h-0 flex-1"
          />
        </div>
        <div className="min-h-0 overflow-y-auto p-4">
          {selected && <Diff key={selected.name} file={selected} split />}
        </div>
        <aside className="min-h-0 overflow-y-auto border-l border-line bg-surface p-4">
          <h3 className="mb-2 text-xs font-medium text-muted">Checks</h3>
          <CheckList runs={x.checkRuns} />
          <h3 className="mt-6 mb-3 text-xs font-medium text-muted">Timeline</h3>
          <Timeline items={x.timeline} dense />
        </aside>
      </div>
    </div>
  );
}
