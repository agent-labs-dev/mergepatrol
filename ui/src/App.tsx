import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import {
  Link,
  Navigate,
  Outlet,
  useMatchRoute,
  useNavigate,
  useParams,
} from "@tanstack/react-router";
import { useEffect, useState } from "react";
import { resetAnalytics, useAnalytics, useWorkspaceAnalytics } from "./analytics";
import { fetchSession, fetchSettings, logout } from "./api";
import type { SessionView } from "./api/SessionView";
import type { WorkspaceView } from "./api/WorkspaceView";
import { Landing } from "./Landing";
import { CreateApp, Install, UnlockSetup } from "./Setup";
import { type ThemeProps, useTheme } from "./theme";
import { Avatar, feedbackUrl, Mark, ThemeToggle } from "./ui";
import { WorkspaceContext } from "./workspace";

/** Setup and sign-in errors come back from the server as `?error=`, read before the router starts. */
const redirectError = new URLSearchParams(location.search).get("error");
if (redirectError) history.replaceState(null, "", location.pathname);

function useRedirectError() {
  const [error, setError] = useState(redirectError);
  return [error, () => setError(null)] as const;
}

export function App() {
  const session = useQuery({ queryKey: ["session"], queryFn: fetchSession });
  useAnalytics(session.data);
  const [error, dismiss] = useRedirectError();
  const [theme, setTheme] = useTheme();

  return (
    <>
      {error && (
        <div className="flex justify-between border-b border-red-200 bg-red-50 px-5 py-2 text-sm text-red-800 dark:border-red-400/30 dark:bg-red-400/10 dark:text-red-200">
          <span>{error}</span>
          <button type="button" onClick={dismiss} className="font-medium hover:opacity-70">
            Dismiss
          </button>
        </div>
      )}
      {session.error && <p className="p-8 text-sm text-bad">{session.error.message}</p>}
      {session.data && <Screen session={session.data} theme={theme} setTheme={setTheme} />}
    </>
  );
}

function Screen({ session, ...theme }: { session: SessionView } & ThemeProps) {
  if (session.status === "needs_setup") {
    return session.setup_unlocked ? <CreateApp /> : <UnlockSetup />;
  }
  if (session.status === "signed_out") return <Landing {...theme} />;
  if (session.status === "signed_in" && session.app) {
    return session.workspaces.length === 0 ? (
      <Install app={session.app} />
    ) : (
      <Dashboard session={session} {...theme} />
    );
  }
  return null;
}

const tabs = [
  ["Queue", "/$account/queue"],
  ["History", "/$account/history"],
  ["Repos", "/$account/repos"],
  ["Settings", "/$account/settings"],
] as const;

/** The workspace opened last, for a path that names none. Falls back to the first one. */
function remembered(workspaces: WorkspaceView[]) {
  const stored = Number(localStorage.getItem("workspace"));
  return workspaces.find((w) => w.installation_id === stored) ?? workspaces[0];
}

function Dashboard({ session, theme, setTheme }: { session: SessionView } & ThemeProps) {
  const queryClient = useQueryClient();
  const navigate = useNavigate();
  const { account } = useParams({ strict: false });
  const current = session.workspaces.find((w) => w.account === account);
  useEffect(() => {
    if (current) localStorage.setItem("workspace", String(current.installation_id));
  }, [current]);
  const signOut = useMutation({
    mutationFn: logout,
    onSuccess: () => {
      resetAnalytics();
      return queryClient.invalidateQueries({ queryKey: ["session"] });
    },
  });
  useWorkspaceAnalytics(session, current);
  const app = session.app;
  if (!app) return null;
  if (!current)
    return (
      <Navigate
        to="/$account/queue"
        params={{ account: remembered(session.workspaces).account }}
        replace
      />
    );

  return (
    <div className="flex h-screen flex-col">
      <NoKeyBanner workspace={current.installation_id} account={current.account} />
      <header className="border-b border-line bg-surface">
        <div className="flex h-12 items-center gap-3 px-5">
          <Mark />
          <span className="text-faint">/</span>
          <Avatar src={current.avatar_url} alt={current.account} />
          <select
            value={current.account}
            onChange={(e) => {
              if (e.target.value === "install") location.href = app.install_url;
              // The same page in the other workspace, without this one's selection.
              else navigate({ to: ".", params: { account: e.target.value }, search: {} });
            }}
            className="field-sizing-content bg-transparent font-medium outline-none"
          >
            {session.workspaces.map((w) => (
              <option key={w.installation_id} value={w.account}>
                {w.account}
              </option>
            ))}
            <option value="install">Add a workspace…</option>
          </select>
          <div className="ml-auto flex items-center gap-3">
            <a
              className="text-muted hover:text-fg"
              href={feedbackUrl}
              target="_blank"
              rel="noreferrer"
              title="Report a bug, request a feature, or tell us what you think"
            >
              Feedback
            </a>
            <ThemeToggle theme={theme} onChange={setTheme} />
            {session.user && (
              <>
                <Avatar src={session.user.avatar_url} alt={session.user.login} size="size-7" />
                <button
                  type="button"
                  className="text-muted hover:text-fg"
                  onClick={() => signOut.mutate()}
                >
                  Sign out
                </button>
              </>
            )}
          </div>
        </div>
        <nav className="flex gap-5 px-5">
          {tabs.map(([label, to]) => (
            <Link
              key={to}
              to={to}
              params={{ account: current.account }}
              className="-mb-px border-b-2 pb-2.5"
              activeProps={{ className: "border-fg text-fg" }}
              inactiveProps={{ className: "border-transparent text-muted hover:text-fg" }}
            >
              {label}
            </Link>
          ))}
        </nav>
      </header>
      <main className="min-h-0 flex-1">
        <WorkspaceContext value={{ workspace: current.installation_id, app }}>
          <Outlet />
        </WorkspaceContext>
      </main>
    </div>
  );
}

/** Across every page while the workspace has no Anthropic API key, until one is added. */
function NoKeyBanner({ workspace, account }: { workspace: number; account: string }) {
  const settings = useQuery({
    queryKey: ["settings", workspace],
    queryFn: () => fetchSettings(workspace),
  });
  const onSettings = useMatchRoute()({ to: "/$account/settings" });
  if (!settings.data || settings.data.anthropic_api_key) return null;
  return (
    <div className="flex items-center gap-3 bg-amber-400 px-5 py-2 text-sm text-amber-950">
      <svg viewBox="0 0 16 16" className="size-4 shrink-0 fill-current" aria-hidden>
        <path d="M8.9 1.5a1 1 0 0 0-1.8 0L.6 13a1 1 0 0 0 .9 1.5h13a1 1 0 0 0 .9-1.5L8.9 1.5ZM7.25 6h1.5v4h-1.5V6Zm0 5.25h1.5v1.5h-1.5v-1.5Z" />
      </svg>
      <span>
        <span className="font-semibold">No Anthropic API key.</span> Without it, Mergepatrol can't
        resolve rebase conflicts or fix failed checks, so those PRs leave the queue.
      </span>
      {!onSettings && (
        <Link
          to="/$account/settings"
          params={{ account }}
          className="ml-auto shrink-0 rounded-md bg-amber-950 px-3 py-1 font-medium text-amber-50 hover:bg-amber-900"
        >
          Add a key →
        </Link>
      )}
    </div>
  );
}
