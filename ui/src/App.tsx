import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import { useState } from "react";
import { fetchSession, logout } from "./api";
import type { SessionView } from "./api/SessionView";
import type { WorkspaceView } from "./api/WorkspaceView";
import { Landing } from "./Landing";
import { Queue } from "./Queue";
import { Repos } from "./Repos";
import { Settings } from "./Settings";
import { CreateApp, Install, UnlockSetup } from "./Setup";
import { type ThemeProps, useTheme } from "./theme";
import { Avatar, feedbackUrl, Mark, ThemeToggle } from "./ui";

/** Setup and sign-in errors come back from the server as `?error=`. */
function useRedirectError() {
  const [error, setError] = useState(() => new URLSearchParams(location.search).get("error"));
  if (error && location.search) history.replaceState(null, "", location.pathname);
  return [error, () => setError(null)] as const;
}

export function App() {
  const session = useQuery({ queryKey: ["session"], queryFn: fetchSession });
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

const tabs = ["Queue", "Repos", "Settings"] as const;

/** The workspace in view, kept across reloads. Falls back to the first one. */
function useWorkspace(workspaces: WorkspaceView[]) {
  const stored = Number(localStorage.getItem("workspace"));
  const [id, setId] = useState(
    workspaces.some((w) => w.installation_id === stored) ? stored : workspaces[0].installation_id,
  );
  const choose = (next: number) => {
    localStorage.setItem("workspace", String(next));
    setId(next);
  };
  return [id, choose] as const;
}

function Dashboard({ session, theme, setTheme }: { session: SessionView } & ThemeProps) {
  const queryClient = useQueryClient();
  const [tab, setTab] = useState<(typeof tabs)[number]>("Queue");
  const [workspace, setWorkspace] = useWorkspace(session.workspaces);
  const signOut = useMutation({
    mutationFn: logout,
    onSuccess: () => queryClient.invalidateQueries({ queryKey: ["session"] }),
  });
  const app = session.app;
  if (!app) return null;
  const current = session.workspaces.find((w) => w.installation_id === workspace);

  return (
    <div className="flex h-screen flex-col">
      <header className="border-b border-line bg-surface">
        <div className="flex h-12 items-center gap-3 px-5">
          <Mark />
          <span className="text-faint">/</span>
          {current && <Avatar src={current.avatar_url} alt={current.account} />}
          <select
            value={workspace}
            onChange={(e) => {
              if (e.target.value === "install") location.href = app.install_url;
              else setWorkspace(Number(e.target.value));
            }}
            className="field-sizing-content bg-transparent font-medium outline-none"
          >
            {session.workspaces.map((w) => (
              <option key={w.installation_id} value={w.installation_id}>
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
          {tabs.map((t) => (
            <button
              key={t}
              type="button"
              onClick={() => setTab(t)}
              className={`-mb-px border-b-2 pb-2.5 ${
                t === tab ? "border-fg text-fg" : "border-transparent text-muted hover:text-fg"
              }`}
            >
              {t}
            </button>
          ))}
        </nav>
      </header>
      <main className="min-h-0 flex-1">
        {tab === "Queue" && <Queue workspace={workspace} />}
        {tab === "Repos" && <Repos workspace={workspace} app={app} />}
        {tab === "Settings" && <Settings workspace={workspace} />}
      </main>
    </div>
  );
}
