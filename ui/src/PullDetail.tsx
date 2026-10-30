import { useQuery } from "@tanstack/react-query";
import { lazy, type ReactNode, Suspense, useState } from "react";
import { fetchPullDetail } from "./api";
import type { CheckRunStateView } from "./api/CheckRunStateView";
import type { CheckRunView } from "./api/CheckRunView";
import type { OpenPullView } from "./api/OpenPullView";
import type { TimelineItemView } from "./api/TimelineItemView";
import { Avatar, timeAgo } from "./ui";

const PullCode = lazy(() => import("./PullCode"));

const tabs = ["summary", "timeline", "code"] as const;
type Tab = (typeof tabs)[number];

/** An open PR as a reviewer looks at it: its summary, timeline and code, read from GitHub as
 * the user. The caller's `actions` sit by its title, and its `footer` under it all. */
export function PullDetail({
  workspace,
  repo,
  trunk,
  pull: p,
  actions,
  footer,
  onBack,
}: {
  workspace: number;
  repo: string;
  trunk: string;
  pull: OpenPullView;
  actions?: ReactNode;
  footer?: ReactNode;
  /** Back to the PRs, on a screen too narrow to show them beside it. */
  onBack: () => void;
}) {
  const [tab, setTab] = useState<Tab>("summary");
  const detail = useQuery({
    queryKey: ["pull", workspace, repo, p.number],
    queryFn: () => fetchPullDetail(workspace, repo, p.number),
    // As the open PRs: read as the user, on their own rate limit.
    refetchInterval: 30_000,
  });
  const d = detail.data;
  const commits = (d?.timeline ?? []).flatMap((t) =>
    t.event.kind === "commit" ? [{ sha: t.event.sha, headline: t.event.headline }] : [],
  );
  return (
    <div className="flex h-full flex-col">
      <div className="border-b border-line px-4 pt-4 pb-3 sm:px-6">
        <button type="button" onClick={onBack} className="mb-3 text-muted hover:text-fg lg:hidden">
          ← {repo.split("/")[1]}
        </button>
        <div className="flex items-center justify-between gap-3">
          <a
            href={p.url}
            target="_blank"
            rel="noreferrer"
            className="truncate text-muted hover:text-fg"
          >
            {repo} <span className="text-ok">#{p.number}</span> ↗
          </a>
          {actions && <span className="flex shrink-0 gap-2">{actions}</span>}
        </div>
        <h2 className="mt-2 text-lg font-semibold tracking-tight">
          {p.title}
          {p.draft && (
            <span className="ml-2 rounded-md bg-sunken px-1.5 py-0.5 align-middle text-xs font-medium text-muted">
              Draft
            </span>
          )}
        </h2>
        <div className="mt-1.5 flex flex-wrap items-center justify-between gap-2 text-sm text-muted">
          <span className="flex items-center gap-1.5">
            {p.author && <Avatar src={p.author.avatar_url} alt={p.author.login} size="size-4" />}
            <span className="text-fg-2">{p.author?.login ?? "ghost"}</span> · opened{" "}
            {timeAgo(p.created_at)}
          </span>
          <code className="font-mono text-xs">gh pr checkout {p.number}</code>
        </div>
        <div className="mt-3 flex items-center justify-between gap-3 font-mono text-xs">
          <span className="truncate">
            <span className="text-fg-2">{trunk}</span> <span className="text-faint">←</span>{" "}
            <span className="text-muted">{p.head_ref}</span>
          </span>
          <span className="shrink-0 text-muted">
            {p.changed_files} {p.changed_files === 1 ? "file" : "files"}{" "}
            <span className="text-ok">+{p.additions}</span>{" "}
            <span className="text-bad">−{p.deletions}</span>
          </span>
        </div>
        <div className="mt-3 flex items-center justify-between gap-3">
          <div className="flex gap-1 rounded-lg bg-sunken p-1">
            {tabs.map((t) => (
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
          {d && ran(d.checks) > 0 && (
            <button
              type="button"
              onClick={() => {
                setTab("summary");
                requestAnimationFrame(() =>
                  document.getElementById("checks")?.scrollIntoView({ behavior: "smooth" }),
                );
              }}
              className="text-sm whitespace-nowrap text-muted hover:text-fg"
            >
              <ChecksMark checks={d.checks} /> {passed(d.checks)} of {ran(d.checks)} passing
            </button>
          )}
        </div>
      </div>
      {tab === "code" ? (
        <Suspense fallback={<p className="px-4 py-6 text-faint sm:px-6">Loading the changes…</p>}>
          <PullCode workspace={workspace} repo={repo} number={p.number} commits={commits} />
        </Suspense>
      ) : (
        <div className="min-h-0 flex-1 overflow-y-auto px-4 py-5 sm:px-6">
          {detail.isPending && <p className="text-faint">Loading…</p>}
          {detail.isError && <p className="text-bad">{detail.error.message}</p>}
          {d && tab === "summary" && (
            <>
              <dl className="grid grid-cols-[100px_1fr] items-center gap-x-4 gap-y-3 text-sm">
                <dt className="text-muted">Reviewers</dt>
                <dd>
                  <Reviewers pull={p} />
                </dd>
                <dt className="text-muted">Labels</dt>
                <dd className="flex flex-wrap gap-1.5">
                  {d.labels.length === 0 && <span className="text-faint">None</span>}
                  {d.labels.map((l) => (
                    <span
                      key={l.name}
                      className="rounded-full px-2 py-0.5 text-xs font-medium ring-1 ring-inset"
                      style={{
                        color: `color-mix(in oklab, #${l.color} 70%, var(--fg))`,
                        backgroundColor: `color-mix(in oklab, #${l.color} 15%, transparent)`,
                        // A ring in the label's color, as GitHub draws it.
                        ["--tw-ring-color" as string]: `color-mix(in oklab, #${l.color} 40%, transparent)`,
                      }}
                    >
                      {l.name}
                    </span>
                  ))}
                </dd>
              </dl>
              <Section title="Description">
                {d.body_html ? (
                  <Markdown html={d.body_html} />
                ) : (
                  <p className="text-faint">No description.</p>
                )}
              </Section>
              <Section id="checks" title="Checks">
                {d.checks.length === 0 ? (
                  <p className="text-faint">No checks.</p>
                ) : (
                  <Checks checks={d.checks} />
                )}
              </Section>
              <Comments timeline={d.timeline} />
            </>
          )}
          {d && tab === "timeline" && <Timeline items={d.timeline} />}
        </div>
      )}
      {footer}
    </div>
  );
}

const passed = (checks: CheckRunView[]) => checks.filter((c) => c.state === "passed").length;
/** The runs that count: a skipped one neither passes nor fails, as GitHub counts them. */
const ran = (checks: CheckRunView[]) => checks.filter((c) => c.state !== "skipped").length;

function ChecksMark({ checks }: { checks: CheckRunView[] }) {
  if (checks.some((c) => c.state === "failed")) return <span className="text-bad">✕</span>;
  if (checks.some((c) => c.state === "pending")) return <span className="text-warn">●</span>;
  return <span className="text-ok">✓</span>;
}

function Reviewers({ pull: p }: { pull: OpenPullView }) {
  const decision = p.review_decision && reviewDecision[p.review_decision];
  if (!p.reviews.length && !p.requested.length && !decision)
    return <span className="text-faint">None</span>;
  return (
    <span className="flex flex-wrap items-center gap-1.5">
      {p.reviews.map((r) => (
        <span
          key={r.reviewer.login}
          className="relative"
          title={`${r.reviewer.login} ${r.approved ? "approved" : "requested changes"}`}
        >
          <Avatar src={r.reviewer.avatar_url} alt={r.reviewer.login} />
          <span
            className={`absolute -right-1 -bottom-1 text-[10px] ${r.approved ? "text-ok" : "text-bad"}`}
          >
            {r.approved ? "✓" : "✕"}
          </span>
        </span>
      ))}
      {p.requested.map((r) => (
        <span key={r.login} className="opacity-50" title={`${r.login} has not reviewed yet`}>
          <Avatar src={r.avatar_url} alt={r.login} />
        </span>
      ))}
      {decision && <span className={`ml-2 text-xs ${decision[1]}`}>{decision[0]}</span>}
    </span>
  );
}

const reviewDecision = {
  approved: ["Approved", "text-ok"],
  review_required: ["Review required", "text-warn"],
  changes_requested: ["Changes requested", "text-bad"],
} as const;

/** A part of the summary that folds away from its title. Open to start with. */
function Section({
  id,
  title,
  actions,
  children,
}: {
  id?: string;
  title: string;
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
          aria-expanded={open}
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

/** GitHub's own rendering of some Markdown, sanitized by GitHub. Its links open in a new tab. */
function Markdown({ html }: { html: string }) {
  return (
    <div
      // Remounted with new HTML, so each version's links get the ref.
      key={html}
      ref={newTabLinks}
      className="markdown"
      // biome-ignore lint/security/noDangerouslySetInnerHtml: GitHub sanitizes `bodyHTML`, as it does for github.com.
      dangerouslySetInnerHTML={{ __html: html }}
    />
  );
}

const newTabLinks = (el: HTMLElement | null) => {
  for (const a of el?.querySelectorAll("a") ?? []) {
    a.target = "_blank";
    a.rel = "noreferrer";
  }
};

const checkState: Record<CheckRunStateView, [string, string, string]> = {
  passed: ["Passed", "text-ok", "M5.5 8.2l1.8 1.8 3.2-3.6"],
  failed: ["Failed", "text-bad", "M6 6l4 4M10 6l-4 4"],
  pending: ["Running", "text-warn", "M8 5v3l2 1.5"],
  skipped: ["Skipped", "text-faint", "M5.5 8h5"],
};

const checkOrder: Record<CheckRunStateView, number> = {
  failed: 0,
  pending: 1,
  passed: 2,
  skipped: 3,
};

/** Failed first: what someone opening a PR looks for. */
function Checks({ checks }: { checks: CheckRunView[] }) {
  return (
    <ul className="space-y-0.5">
      {[...checks]
        .sort((a, b) => checkOrder[a.state] - checkOrder[b.state])
        .map((c) => {
          const [label, color, d] = checkState[c.state];
          const took = duration(c);
          const row = (
            <>
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
              <span className="min-w-0 flex-1 truncate">{c.name}</span>
              {took && <span className="text-xs text-faint tabular-nums">{took}</span>}
              <span
                className={`w-16 text-right text-sm ${c.state === "passed" ? "text-muted" : color}`}
              >
                {label}
              </span>
            </>
          );
          const style = "flex items-center gap-3 rounded-md px-2 py-1.5";
          return (
            <li key={`${c.name}${c.url}`}>
              {c.url ? (
                <a
                  href={c.url}
                  target="_blank"
                  rel="noreferrer"
                  className={`${style} hover:bg-sunken`}
                >
                  {row}
                </a>
              ) : (
                <span className={style}>{row}</span>
              )}
            </li>
          );
        })}
    </ul>
  );
}

/** How long a finished run took, as `4m 10s`. */
function duration(c: CheckRunView) {
  if (!c.started_at || !c.completed_at) return null;
  const s = Math.round((Date.parse(c.completed_at) - Date.parse(c.started_at)) / 1000);
  if (!(s >= 0)) return null;
  return s < 60 ? `${s}s` : `${Math.floor(s / 60)}m ${s % 60}s`;
}

const commentKinds = { all: "Filter", comment: "Comments", review: "Reviews" } as const;
type CommentKind = keyof typeof commentKinds;

/** The conversation: comments, and reviews that say something. */
function Comments({ timeline }: { timeline: TimelineItemView[] }) {
  const [kind, setKind] = useState<CommentKind>("all");
  const [newest, setNewest] = useState(true);
  const comments = timeline.flatMap((t) =>
    t.event.kind === "comment" || (t.event.kind === "review" && t.event.body_html)
      ? [{ ...t, kind: t.event.kind, html: t.event.body_html }]
      : [],
  );
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
                onChange={(e) => setKind(e.target.value as CommentKind)}
                aria-label="Show"
                // The global select chevron is unlayered, so utilities cannot drop it.
                style={{ backgroundImage: "none", paddingRight: 0 }}
                className="h-auto w-auto border-0 bg-transparent p-0 text-sm text-inherit shadow-none"
              >
                {Object.entries(commentKinds).map(([k, v]) => (
                  <option key={k} value={k}>
                    {v}
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
            <li
              key={`${c.at}${c.actor?.login}`}
              className="rounded-lg border border-line bg-surface"
            >
              <p className="flex items-center gap-2 border-b border-line-soft px-3 py-2 text-sm">
                {c.actor && <Avatar src={c.actor.avatar_url} alt={c.actor.login} />}
                <span className="font-medium">{c.actor?.login ?? "ghost"}</span>
                {c.event.kind === "review" && <ReviewState approval={c.event.approval} />}
                <span className="ml-auto text-xs text-faint">{timeAgo(c.at)}</span>
              </p>
              <div className="px-3 py-2.5">
                <Markdown html={c.html} />
              </div>
            </li>
          ))}
        </ol>
      )}
    </Section>
  );
}

const approvals = {
  approved: ["approved", "text-ok"],
  changes_requested: ["requested changes", "text-bad"],
  commented: ["reviewed", "text-muted"],
  dismissed: ["review dismissed", "text-faint"],
} as const;

function ReviewState({ approval }: { approval: keyof typeof approvals }) {
  const [text, color] = approvals[approval];
  return <span className={`text-sm ${color}`}>{text}</span>;
}

function Timeline({ items }: { items: TimelineItemView[] }) {
  if (items.length === 0) return <p className="text-faint">Nothing yet.</p>;
  return (
    <ol className="space-y-4">
      {items.map((t, i) => (
        // biome-ignore lint/suspicious/noArrayIndexKey: events have no ID, and only append.
        <li key={i} className="flex gap-3">
          {t.actor ? (
            <Avatar src={t.actor.avatar_url} alt={t.actor.login} />
          ) : (
            <span className="size-5 shrink-0 rounded-full bg-sunken" />
          )}
          <div className="min-w-0 flex-1">
            <p className="text-sm text-muted">
              <span className="font-medium text-fg">{t.actor?.login ?? "ghost"}</span>{" "}
              <Event item={t} />{" "}
              <span className="whitespace-nowrap text-faint">· {timeAgo(t.at)}</span>
            </p>
            {(t.event.kind === "comment" || t.event.kind === "review") && t.event.body_html && (
              <div className="mt-1.5 rounded-md border border-line bg-surface px-3 py-2">
                <Markdown html={t.event.body_html} />
              </div>
            )}
          </div>
        </li>
      ))}
    </ol>
  );
}

const sha = (s: string) => <code className="font-mono text-xs text-fg-2">{s.slice(0, 7)}</code>;

function Event({ item: { event: e } }: { item: TimelineItemView }) {
  switch (e.kind) {
    case "commit":
      return (
        <>
          pushed {sha(e.sha)} <span className="text-fg-2">{e.headline}</span>
        </>
      );
    case "review":
      return <ReviewState approval={e.approval} />;
    case "comment":
      return <>commented</>;
    case "review_requested":
      return <>requested a review from {e.reviewer}</>;
    case "labeled":
      return (
        <>
          {e.added ? "added" : "removed"}{" "}
          <span className="rounded-full bg-sunken px-1.5 py-0.5 text-xs text-fg-2">
            {e.label.name}
          </span>
        </>
      );
    case "force_pushed":
      return (
        <>
          force-pushed {sha(e.before)} → {sha(e.after)}
        </>
      );
    case "ready_for_review":
      return <>marked it ready for review</>;
    case "converted_to_draft":
      return <>converted it to a draft</>;
  }
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
