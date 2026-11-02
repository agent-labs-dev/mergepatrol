// PROTOTYPE: four layouts for the Repos page, over the same in-memory store. Throwaway.
import { type ReactNode, useState } from "react";
import { field, RepoIcon, secondary } from "../../ui";
import { problem, type Repo, type Store, short } from "./data";

/* ---------- atoms every variant may use ---------- */

const small =
  "inline-flex h-7 items-center justify-center rounded-md px-2.5 text-xs font-medium disabled:opacity-40";
const smallPrimary = `${small} bg-fg text-surface hover:bg-fg-2`;
const smallSecondary = `${small} border border-line-strong bg-surface text-fg-2 hover:bg-page`;

function Switch({ on, onChange, label }: { on: boolean; onChange: (v: boolean) => void; label: string }) {
  return (
    <button
      type="button"
      role="switch"
      aria-checked={on}
      aria-label={label}
      title={on ? "Queuing: click to turn off" : "Not queuing: click to turn on"}
      onClick={(e) => {
        e.stopPropagation();
        onChange(!on);
      }}
      className={`relative h-5 w-9 shrink-0 rounded-full transition-colors ${on ? "bg-ok" : "bg-line-strong"}`}
    >
      <span
        className={`absolute top-0.5 size-4 rounded-full bg-white shadow transition-all ${on ? "left-4.5" : "left-0.5"}`}
      />
    </button>
  );
}

function Badge({ tone, children }: { tone: "ok" | "warn" | "bad" | "muted"; children: ReactNode }) {
  const tones = {
    ok: "bg-ok/10 text-ok",
    warn: "bg-warn/10 text-warn",
    bad: "bg-bad/10 text-bad",
    muted: "bg-sunken text-muted",
  };
  return (
    <span className={`inline-flex h-5 items-center rounded px-1.5 text-[11px] font-medium whitespace-nowrap ${tones[tone]}`}>
      {children}
    </span>
  );
}

const Private = () => <span className="text-xs text-faint">Private</span>;

function PrLink({ n }: { n: number }) {
  return (
    <a href="#pr" className="text-xs font-medium text-fg underline underline-offset-2" onClick={(e) => e.preventDefault()}>
      PR #{n}
    </a>
  );
}

/** Stands in for the real RepoSettings panel, which needs the API. */
function SettingsStub({ repo }: { repo: string }) {
  return (
    <div className="border-t border-line-soft bg-page/60 px-4 py-4 text-xs text-muted">
      <div className="mb-2 font-medium text-fg-2">Settings for {short(repo)}</div>
      <div className="grid grid-cols-2 gap-2 sm:grid-cols-4">
        {["Merge method", "Batch size", "Required checks", "Model"].map((s) => (
          <div key={s} className="rounded-md border border-dashed border-line-strong px-2 py-3 text-center">
            {s}
          </div>
        ))}
      </div>
      <div className="mt-2 text-faint">The existing RepoSettings panel mounts here.</div>
    </div>
  );
}

const Intro = ({ wide }: { wide?: boolean }) => (
  <>
    <h1 className="text-2xl font-semibold tracking-tight">Repos</h1>
    <p className={`mt-1 mb-6 text-muted ${wide ? "max-w-2xl" : ""}`}>
      The repos the App can access. A repo queues once its default branch has{" "}
      <code className="rounded bg-sunken px-1 text-xs">.mergepatrol/mergepatrol.yml</code>. To add
      repos,{" "}
      <a href="#install" className="font-medium text-fg underline underline-offset-2">
        change the installation on GitHub
      </a>
      .
    </p>
  </>
);

/** The one action a repo's state calls for, as a small button, or its open PR. */
function FixButton({ r, store, kind = "primary" }: { r: Repo; store: Store; kind?: "primary" | "secondary" }) {
  const p = problem(r);
  if (r.pr) return <PrLink n={r.pr} />;
  if (!p) return null;
  if (r.on && r.config_error)
    return (
      <a href="#file" className={kind === "primary" ? smallPrimary : smallSecondary}>
        Open file
      </a>
    );
  return (
    <button
      type="button"
      className={kind === "primary" ? smallPrimary : smallSecondary}
      onClick={(e) => {
        e.stopPropagation();
        store.openPr([r.repo]);
      }}
    >
      {p.action}
    </button>
  );
}

function Override({ r, store }: { r: Repo; store: Store }) {
  if (r.enabled === null) return null;
  return (
    <span className="text-xs text-muted">
      Forced {r.enabled ? "on" : "off"} ·{" "}
      <button
        type="button"
        onClick={(e) => {
          e.stopPropagation();
          store.follow(r.repo);
        }}
        className="font-medium text-fg underline underline-offset-2"
      >
        follow config
      </button>
    </span>
  );
}

const matches = (r: Repo, q: string) => r.repo.includes(q.trim());

/* ---------- A: dense table ---------- */

/** One line per repo. State gets its own columns; the switch replaces Turn on/off; the one
 * needed fix sits in the column of the state it fixes; Settings is the row itself. */
export function VariantA({ store }: { store: Store }) {
  const [q, setQ] = useState("");
  const [open, setOpen] = useState<string | null>(null);
  const shown = store.repos.filter((r) => matches(r, q));
  const group = (title: string, rows: Repo[]) => (
    <>
      <tr>
        <th colSpan={5} className="bg-sunken/60 px-4 py-1.5 text-left text-xs font-medium text-muted">
          {title} · {rows.length}
        </th>
      </tr>
      {rows.map((r) => (
        <Row key={r.repo} r={r} />
      ))}
    </>
  );
  const Row = ({ r }: { r: Repo }) => (
    <>
      <tr
        onClick={() => setOpen(open === r.repo ? null : r.repo)}
        className={`cursor-pointer border-t border-line-soft hover:bg-page ${open === r.repo ? "bg-page" : ""}`}
      >
        <td className="py-2 pr-2 pl-4">
          <Switch on={r.on} onChange={(v) => store.setOn([r.repo], v)} label={`Queue ${r.repo}`} />
        </td>
        <td className="py-2 pr-3">
          <div className="flex items-center gap-2">
            <RepoIcon repo={r.repo} />
            <span className="font-medium">{short(r.repo)}</span>
            {r.private && <Private />}
          </div>
          {(r.enabled !== null || (r.on && r.config_error)) && (
            <div className="mt-0.5 pl-7">
              {r.on && r.config_error ? (
                <span className="text-xs text-bad" title={r.config_error}>
                  Paused: {r.config_error}
                </span>
              ) : (
                <Override r={r} store={store} />
              )}
            </div>
          )}
        </td>
        <td className="py-2 pr-3">
          {r.on && r.config_error ? (
            <Badge tone="bad">Invalid</Badge>
          ) : r.has_config ? (
            <Badge tone="ok">Config</Badge>
          ) : r.pr ? (
            <PrLink n={r.pr} />
          ) : (
            <button type="button" className={smallSecondary} onClick={(e) => (e.stopPropagation(), store.openPr([r.repo]))}>
              Set up
            </button>
          )}
        </td>
        <td className="py-2 pr-3">
          {r.skill === null ? (
            <span className="text-faint">—</span>
          ) : r.skill === "current" ? (
            <Badge tone="ok">Current</Badge>
          ) : r.pr ? (
            <PrLink n={r.pr} />
          ) : (
            <button type="button" className={smallSecondary} onClick={(e) => (e.stopPropagation(), store.openPr([r.repo]))}>
              {r.skill === "missing" ? "Add" : "Update"}
            </button>
          )}
        </td>
        <td className="py-2 pr-4 text-right text-faint">{open === r.repo ? "▾" : "›"}</td>
      </tr>
      {open === r.repo && (
        <tr>
          <td colSpan={5} className="p-0">
            <SettingsStub repo={r.repo} />
          </td>
        </tr>
      )}
    </>
  );
  return (
    <div className="mx-auto max-w-3xl px-4 py-10 sm:px-6">
      <Intro />
      <input value={q} onChange={(e) => setQ(e.target.value)} placeholder="Filter repos" className={`${field} w-full`} />
      <div className="mt-4 overflow-hidden rounded-lg border border-line bg-surface">
        <table className="w-full text-sm">
          <thead>
            <tr className="text-left text-xs text-faint">
              <th className="w-14 py-2 pl-4 font-normal">Queue</th>
              <th className="py-2 font-normal">Repo</th>
              <th className="w-24 py-2 font-normal">Config</th>
              <th className="w-24 py-2 font-normal">Skill</th>
              <th className="w-8" />
            </tr>
          </thead>
          <tbody>
            {group("Queuing", shown.filter((r) => r.on))}
            {group("Available", shown.filter((r) => !r.on))}
          </tbody>
        </table>
      </div>
    </div>
  );
}

/* ---------- B: needs attention first ---------- */

/** A to-do list on top, grouped by fix with one button per group; below it, every repo once,
 * stripped to name and switch, Available folded away. */
export function VariantB({ store }: { store: Store }) {
  const [q, setQ] = useState("");
  const [open, setOpen] = useState<string | null>(null);
  const [showAvailable, setShowAvailable] = useState(false);
  const todo = store.repos.filter((r) => problem(r) && !r.pr);
  const buckets = [
    { key: "Config doesn't parse", note: "The queue is paused until the config file parses again." },
    { key: "No config file", note: "Turned on here, so it queues on defaults. Set up opens a PR adding the file." },
    { key: "No skill", note: "Agents in these repos don't know MergeLoop. Opens one PR per repo." },
    { key: "Skill outdated", note: "The agent skill is older than MergeLoop's. Opens one PR per repo." },
  ]
    .map((b) => ({ ...b, rows: todo.filter((r) => problem(r)?.label === b.key) }))
    .filter((b) => b.rows.length > 0);
  const queuing = store.repos.filter((r) => r.on && matches(r, q));
  const available = store.repos.filter((r) => !r.on && matches(r, q));

  const line = (r: Repo) => (
    <div key={r.repo}>
      <div
        onClick={() => setOpen(open === r.repo ? null : r.repo)}
        className="flex cursor-pointer items-center gap-3 px-4 py-2 hover:bg-page"
      >
        <RepoIcon repo={r.repo} />
        <span className="min-w-0 truncate">{short(r.repo)}</span>
        {r.private && <Private />}
        {r.pr && <PrLink n={r.pr} />}
        {problem(r) && !r.pr && <span className="size-1.5 rounded-full bg-warn" title={problem(r)?.label} />}
        <span className="ml-auto flex items-center gap-3">
          <Override r={r} store={store} />
          {!r.has_config && !r.on && !r.pr && (
            <button type="button" className={smallSecondary} onClick={(e) => (e.stopPropagation(), store.openPr([r.repo]))}>
              Set up
            </button>
          )}
          <Switch on={r.on} onChange={(v) => store.setOn([r.repo], v)} label={`Queue ${r.repo}`} />
        </span>
      </div>
      {open === r.repo && <SettingsStub repo={r.repo} />}
    </div>
  );

  return (
    <div className="mx-auto max-w-2xl px-4 py-10 sm:px-6">
      <Intro />
      {buckets.length > 0 ? (
        <section className="rounded-lg border border-warn/30 bg-warn/5">
          <h2 className="px-4 pt-3 text-xs font-medium text-warn">Needs attention · {todo.length}</h2>
          <div className="divide-y divide-warn/15">
            {buckets.map((b) => (
              <details key={b.key} className="group px-4 py-3" open={b.rows.length <= 3}>
                <summary className="flex cursor-pointer list-none items-center gap-3">
                  <span className="text-faint group-open:rotate-90">›</span>
                  <span className="font-medium">
                    {b.key} <span className="font-normal text-muted">· {b.rows.length}</span>
                  </span>
                  {b.rows.length === 1 ? (
                    <span className="ml-auto" onClick={(e) => e.preventDefault()}>
                      <FixButton r={b.rows[0]} store={store} />
                    </span>
                  ) : (
                    <button
                      type="button"
                      className={`${smallPrimary} ml-auto`}
                      onClick={(e) => {
                        e.preventDefault();
                        store.openPr(b.rows.map((r) => r.repo));
                      }}
                    >
                      {problem(b.rows[0])?.action} in {b.rows.length}
                    </button>
                  )}
                </summary>
                <p className="mt-1 pl-5 text-xs text-muted">{b.note}</p>
                <div className="mt-2 flex flex-wrap gap-1.5 pl-5">
                  {b.rows.map((r) => (
                    <span key={r.repo} className="inline-flex items-center gap-1.5 rounded-md bg-surface px-2 py-1 text-xs ring-1 ring-line">
                      {short(r.repo)}
                      <button
                        type="button"
                        className="text-muted hover:text-fg"
                        title={problem(r)?.action}
                        onClick={() => store.openPr([r.repo])}
                      >
                        ↗
                      </button>
                    </span>
                  ))}
                </div>
              </details>
            ))}
          </div>
        </section>
      ) : (
        <p className="rounded-lg border border-ok/30 bg-ok/5 px-4 py-3 text-ok">Every queuing repo is set up and current.</p>
      )}

      <input value={q} onChange={(e) => setQ(e.target.value)} placeholder="Filter repos" className={`${field} mt-8 w-full`} />
      <h2 className="mt-6 mb-2 text-xs font-medium text-muted">Queuing · {queuing.length}</h2>
      <div className="divide-y divide-line-soft overflow-hidden rounded-lg border border-line bg-surface">
        {queuing.map(line)}
      </div>
      <button
        type="button"
        onClick={() => setShowAvailable(!showAvailable)}
        className="mt-6 mb-2 flex items-center gap-1.5 text-xs font-medium text-muted hover:text-fg"
      >
        <span className={showAvailable || q ? "rotate-90" : ""}>›</span> Available · {available.length}
        <span className="font-normal text-faint">not queuing</span>
      </button>
      {(showAvailable || q) && (
        <div className="divide-y divide-line-soft overflow-hidden rounded-lg border border-line bg-surface">
          {available.map(line)}
        </div>
      )}
    </div>
  );
}

/* ---------- C: list + detail ---------- */

/** Pick a repo on the left; its whole state and every action live on the right. Rows carry no
 * buttons at all. */
export function VariantC({ store }: { store: Store }) {
  const [q, setQ] = useState("");
  const [sel, setSel] = useState(store.repos[0].repo);
  const r = store.repos.find((x) => x.repo === sel) ?? store.repos[0];
  const item = (x: Repo) => {
    const p = problem(x);
    return (
      <button
        key={x.repo}
        type="button"
        onClick={() => setSel(x.repo)}
        className={`flex w-full items-center gap-2 rounded-md px-2 py-1.5 text-left ${x.repo === sel ? "bg-sunken text-fg" : "text-fg-2 hover:bg-sunken/60"}`}
      >
        <RepoIcon repo={x.repo} size="size-4" />
        <span className="min-w-0 flex-1 truncate">{short(x.repo)}</span>
        {x.pr ? (
          <span className="text-[11px] text-faint">#{x.pr}</span>
        ) : p ? (
          <span className={`size-1.5 rounded-full ${p.tone === "bad" ? "bg-bad" : "bg-warn"}`} title={p.label} />
        ) : null}
      </button>
    );
  };
  const fact = (label: string, value: ReactNode, action?: ReactNode) => (
    <div className="flex items-center gap-4 border-t border-line-soft px-5 py-3.5">
      <div className="w-24 shrink-0 text-xs text-muted">{label}</div>
      <div className="min-w-0 flex-1">{value}</div>
      {action}
    </div>
  );
  const queuing = store.repos.filter((x) => x.on && matches(x, q));
  const available = store.repos.filter((x) => !x.on && matches(x, q));
  return (
    <div className="mx-auto max-w-5xl px-4 py-10 sm:px-6">
      <Intro wide />
      <div className="grid grid-cols-[16rem_1fr] gap-6">
        <aside className="h-[calc(100dvh-16rem)] min-h-96 overflow-y-auto">
          <input value={q} onChange={(e) => setQ(e.target.value)} placeholder="Filter repos" className={`${field} mb-3 w-full`} />
          <div className="px-2 pb-1 text-xs font-medium text-muted">Queuing · {queuing.length}</div>
          {queuing.map(item)}
          <div className="mt-4 px-2 pb-1 text-xs font-medium text-muted">Available · {available.length}</div>
          {available.map(item)}
        </aside>
        <section className="self-start overflow-hidden rounded-lg border border-line bg-surface">
          <div className="flex items-center gap-3 px-5 py-4">
            <RepoIcon repo={r.repo} size="size-8" />
            <div>
              <div className="text-base font-semibold">{r.repo}</div>
              <div className="text-xs text-muted">
                {r.private ? "Private" : "Public"} · {r.on ? "Queuing" : "Not queuing"}
              </div>
            </div>
            <a href="#gh" className={`${secondary} ml-auto`}>
              Open on GitHub
            </a>
          </div>
          {fact(
            "Queue",
            <span>
              {r.on ? "On" : "Off"}{" "}
              <span className="text-muted">
                {r.enabled === null ? "· follows the config file" : "· set here, whatever the config file says"}
              </span>
            </span>,
            <span className="flex items-center gap-3">
              {r.enabled !== null && (
                <button type="button" className={smallSecondary} onClick={() => store.follow(r.repo)}>
                  Follow config
                </button>
              )}
              <Switch on={r.on} onChange={(v) => store.setOn([r.repo], v)} label="Queue" />
            </span>,
          )}
          {fact(
            "Config file",
            r.on && r.config_error ? (
              <span className="text-bad">Doesn't parse: {r.config_error}</span>
            ) : r.has_config ? (
              <span>
                <code className="rounded bg-sunken px-1 text-xs">.mergepatrol/mergepatrol.yml</code>
              </span>
            ) : (
              <span className="text-muted">None on the default branch</span>
            ),
            !r.has_config && (r.pr ? <PrLink n={r.pr} /> : <button type="button" className={smallPrimary} onClick={() => store.openPr([r.repo])}>Set up</button>),
          )}
          {fact(
            "Agent skill",
            r.skill === null ? (
              <span className="text-faint">Checked once there is a config file</span>
            ) : r.skill === "current" ? (
              <span className="text-ok">Up to date</span>
            ) : (
              <span className="text-warn">{r.skill === "missing" ? "Not in the repo" : "Older than MergeLoop's"}</span>
            ),
            r.skill && r.skill !== "current" && (r.pr ? <PrLink n={r.pr} /> : <button type="button" className={smallPrimary} onClick={() => store.openPr([r.repo])}>{r.skill === "missing" ? "Add skill" : "Update skill"}</button>),
          )}
          <SettingsStub repo={r.repo} />
        </section>
      </div>
    </div>
  );
}

/* ---------- D: facets + bulk ---------- */

const FACETS: { key: string; test: (r: Repo) => boolean }[] = [
  { key: "All", test: () => true },
  { key: "Queuing", test: (r) => r.on },
  { key: "Available", test: (r) => !r.on },
  { key: "Needs attention", test: (r) => !!problem(r) && !r.pr },
  { key: "Private", test: (r) => r.private },
];

/** One flat list. Chips filter it instead of sections; checkboxes pick repos and a bar acts on
 * all of them at once. Rows say their state in words, with no buttons. */
export function VariantD({ store }: { store: Store }) {
  const [q, setQ] = useState("");
  const [facet, setFacet] = useState("All");
  const [picked, setPicked] = useState<string[]>([]);
  const [open, setOpen] = useState<string | null>(null);
  const test = FACETS.find((f) => f.key === facet)?.test ?? (() => true);
  const shown = store.repos.filter((r) => test(r) && matches(r, q));
  const sel = store.repos.filter((r) => picked.includes(r.repo));
  const toggle = (name: string) => setPicked((p) => (p.includes(name) ? p.filter((x) => x !== name) : [...p, name]));
  const all = shown.length > 0 && shown.every((r) => picked.includes(r.repo));
  const fixable = sel.filter((r) => problem(r) && !r.pr && !(r.on && r.config_error));
  const unset = sel.filter((r) => !r.on && !r.has_config && !r.pr);
  const act = (f: () => void) => () => {
    f();
    setPicked([]);
  };
  return (
    <div className="mx-auto max-w-3xl px-4 py-10 pb-40 sm:px-6">
      <Intro />
      <div className="flex flex-wrap items-center gap-2">
        {FACETS.map((f) => (
          <button
            key={f.key}
            type="button"
            onClick={() => setFacet(f.key)}
            className={`h-7 rounded-full px-3 text-xs font-medium ${facet === f.key ? "bg-fg text-surface" : "bg-sunken text-fg-2 hover:text-fg"}`}
          >
            {f.key} <span className="opacity-60">{store.repos.filter(f.test).length}</span>
          </button>
        ))}
        <input value={q} onChange={(e) => setQ(e.target.value)} placeholder="Filter" className={`${field} ml-auto h-7 w-40 text-xs`} />
      </div>
      <div className="mt-4 overflow-hidden rounded-lg border border-line bg-surface">
        <label className="flex items-center gap-3 border-b border-line px-4 py-2 text-xs text-muted">
          <input type="checkbox" checked={all} onChange={() => setPicked(all ? [] : shown.map((r) => r.repo))} />
          {picked.length ? `${picked.length} selected` : `${shown.length} repos`}
        </label>
        {shown.map((r) => {
          const p = problem(r);
          return (
            <div key={r.repo} className="border-t border-line-soft first:border-t-0">
              <div className={`flex items-center gap-3 px-4 py-2 ${picked.includes(r.repo) ? "bg-sunken/60" : "hover:bg-page"}`}>
                <input type="checkbox" checked={picked.includes(r.repo)} onChange={() => toggle(r.repo)} />
                <span className={`size-2 rounded-full ${r.on ? (r.config_error ? "bg-bad" : "bg-ok") : "bg-line-strong"}`} title={r.on ? "Queuing" : "Not queuing"} />
                <button type="button" className="min-w-0 truncate text-left font-medium" onClick={() => setOpen(open === r.repo ? null : r.repo)}>
                  {short(r.repo)}
                </button>
                {r.private && <Private />}
                <span className="ml-auto flex items-center gap-2">
                  <Override r={r} store={store} />
                  {r.pr ? (
                    <PrLink n={r.pr} />
                  ) : p ? (
                    <Badge tone={p.tone}>{p.label}</Badge>
                  ) : r.has_config ? (
                    <Badge tone="ok">Ready</Badge>
                  ) : (
                    <Badge tone="muted">No config</Badge>
                  )}
                  <button type="button" onClick={() => setOpen(open === r.repo ? null : r.repo)} className="px-1 text-xs text-faint hover:text-fg">
                    Settings
                  </button>
                </span>
              </div>
              {open === r.repo && <SettingsStub repo={r.repo} />}
            </div>
          );
        })}
      </div>
      {sel.length > 0 && (
        <div className="fixed bottom-20 left-1/2 z-40 flex -translate-x-1/2 items-center gap-2 rounded-xl border border-line bg-surface px-3 py-2 shadow-xl">
          <span className="pr-2 text-xs text-muted">{sel.length} selected</span>
          {fixable.length > 0 && (
            <button type="button" className={smallPrimary} onClick={act(() => store.openPr(fixable.map((r) => r.repo)))}>
              Fix {fixable.length} (opens PRs)
            </button>
          )}
          {unset.length > 0 && (
            <button type="button" className={smallPrimary} onClick={act(() => store.openPr(unset.map((r) => r.repo)))}>
              Set up {unset.length} (opens PRs)
            </button>
          )}
          {sel.some((r) => !r.on) && (
            <button type="button" className={smallSecondary} onClick={act(() => store.setOn(sel.filter((r) => !r.on).map((r) => r.repo), true))}>
              Turn on {sel.filter((r) => !r.on).length}
            </button>
          )}
          {sel.some((r) => r.on) && (
            <button type="button" className={smallSecondary} onClick={act(() => store.setOn(sel.filter((r) => r.on).map((r) => r.repo), false))}>
              Turn off {sel.filter((r) => r.on).length}
            </button>
          )}
          <button type="button" className="px-2 text-xs text-muted hover:text-fg" onClick={() => setPicked([])}>
            Clear
          </button>
        </div>
      )}
    </div>
  );
}

