import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import { useState } from "react";
import { fetchSession, logout } from "./api";
import type { SessionView } from "./api/SessionView";
import type { WorkspaceView } from "./api/WorkspaceView";
import { Queue } from "./Queue";
import { Repos } from "./Repos";
import { Settings } from "./Settings";
import { CreateApp, Install, SignIn, UnlockSetup } from "./Setup";

/** Setup and sign-in errors come back from the server as `?error=`. */
function useRedirectError() {
  const [error, setError] = useState(() => new URLSearchParams(location.search).get("error"));
  if (error && location.search) history.replaceState(null, "", location.pathname);
  return [error, () => setError(null)] as const;
}

export function App() {
  const session = useQuery({ queryKey: ["session"], queryFn: fetchSession });
  const [error, dismiss] = useRedirectError();

  return (
    <>
      {error && (
        <div className="flex justify-between bg-red-950 px-4 py-2 text-sm text-red-200">
          <span>{error}</span>
          <button type="button" onClick={dismiss}>
            Dismiss
          </button>
        </div>
      )}
      {session.error && <p className="p-8 text-red-400">{session.error.message}</p>}
      {session.data && <Screen session={session.data} />}
    </>
  );
}

function Screen({ session }: { session: SessionView }) {
  if (session.status === "needs_setup") {
    return session.setup_unlocked ? <CreateApp /> : <UnlockSetup />;
  }
  if (session.status === "signed_out") return <SignIn />;
  if (session.status === "signed_in" && session.app) {
    return session.workspaces.length === 0 ? (
      <Install app={session.app} />
    ) : (
      <Dashboard session={session} />
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

function Dashboard({ session }: { session: SessionView }) {
  const queryClient = useQueryClient();
  const [tab, setTab] = useState<(typeof tabs)[number]>("Queue");
  const [workspace, setWorkspace] = useWorkspace(session.workspaces);
  const signOut = useMutation({
    mutationFn: logout,
    onSuccess: () => queryClient.invalidateQueries({ queryKey: ["session"] }),
  });
  const app = session.app;
  if (!app) return null;

  return (
    <main className="mx-auto max-w-5xl p-8">
      <header className="mb-6 flex items-center justify-between">
        <div className="flex items-baseline gap-6">
          <h1 className="text-2xl font-semibold">Mergepatrol</h1>
          <nav className="flex gap-4 text-sm">
            {tabs.map((t) => (
              <button
                key={t}
                type="button"
                onClick={() => setTab(t)}
                className={t === tab ? "text-white" : "text-neutral-500 hover:text-neutral-300"}
              >
                {t}
              </button>
            ))}
          </nav>
        </div>
        {session.user && (
          <div className="flex items-center gap-3 text-sm">
            <select
              value={workspace}
              onChange={(e) => {
                if (e.target.value === "install") location.href = app.install_url;
                else setWorkspace(Number(e.target.value));
              }}
              className="rounded border border-neutral-700 bg-neutral-900 px-2 py-1"
            >
              {session.workspaces.map((w) => (
                <option key={w.installation_id} value={w.installation_id}>
                  {w.account}
                </option>
              ))}
              <option value="install">Add a workspace…</option>
            </select>
            <img src={session.user.avatar_url} alt="" className="h-6 w-6 rounded-full" />
            <span>{session.user.login}</span>
            <button
              type="button"
              className="text-neutral-500 hover:text-neutral-300"
              onClick={() => signOut.mutate()}
            >
              Sign out
            </button>
          </div>
        )}
      </header>
      {tab === "Queue" && <Queue workspace={workspace} />}
      {tab === "Repos" && <Repos workspace={workspace} app={app} />}
      {tab === "Settings" && <Settings workspace={workspace} />}
    </main>
  );
}
