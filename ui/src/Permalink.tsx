import { useQuery } from "@tanstack/react-query";
import { Navigate, useParams } from "@tanstack/react-router";
import { useEffect } from "react";
import { fetchLanes, fetchPullStatus } from "./api";
import { secondary } from "./ui";

/** A permalink to come back to once signed in, across the sign-in with GitHub. */
const AFTER_SIGN_IN = "after-sign-in";
const PERMALINK = /^\/[^/]+\/[^/]+\/pull\/\d+$/;

/** Signed out on a permalink: keep it for after the sign-in. */
export function rememberPermalink() {
  if (PERMALINK.test(location.pathname)) sessionStorage.setItem(AFTER_SIGN_IN, location.pathname);
}

const params = (path: string | null | undefined) => {
  const [, account, repo, , number] = path?.split("/") ?? [];
  return number ? { account, repo, number } : null;
};

/** The permalink a sign-in started from, if it did. */
export const permalinkAfterSignIn = () => params(sessionStorage.getItem(AFTER_SIGN_IN));

/** The permalink at `path`, if it is one. */
export const permalinkAt = (path: string) => (PERMALINK.test(path) ? params(path) : null);

/** Where `/{owner}/{repo}/pull/{n}` leads: the PR in its lane while queued, its exit in the
 * history once merged or ejected, the repo's settings while Mergepatrol is off there, or else
 * the PR among the repo's open PRs. */
export function Permalink({ workspace }: { workspace: number }) {
  const { account, repo, number } = useParams({ from: "/$account/$repo/pull/$number" });
  const full = `${account}/${repo}`;
  const n = Number(number);
  useEffect(() => sessionStorage.removeItem(AFTER_SIGN_IN), []);
  const status = useQuery({
    queryKey: ["pull-status", full, n],
    queryFn: () => fetchPullStatus(full, n),
    enabled: Number.isInteger(n),
    retry: false,
  });
  const s = status.data;
  const amongOpen = s !== undefined && !s.queue && !s.merged && !s.ejected;
  const lanes = useQuery({
    queryKey: ["lanes", workspace],
    queryFn: () => fetchLanes(workspace),
    enabled: amongOpen && s.mergepatrol.state !== "off",
  });

  if (!Number.isInteger(n) || status.isError)
    return <NotHere account={account} repo={repo} number={number} />;
  if (!s) return null;
  if (s.queue)
    return (
      <Navigate
        to="/$account/queue"
        params={{ account }}
        search={{ lane: `${full}:${s.queue.trunk}`, pr: s.queue.stack?.number ?? n }}
        replace
      />
    );
  if (s.merged || s.ejected)
    return (
      <Navigate
        to="/$account/history"
        params={{ account }}
        search={{ repo: full, pr: n }}
        replace
      />
    );
  if (s.mergepatrol.state === "off")
    return <Navigate to="/$account/repos" params={{ account }} search={{ repo: full }} replace />;
  if (lanes.isPending) return null;
  const lane = lanes.data?.find((l) => l.repo === full);
  return (
    <Navigate
      to="/$account/queue"
      params={{ account }}
      search={{ lane: lane && `${full}:${lane.trunk}`, pr: n, drafts: true }}
      replace
    />
  );
}

/** A permalink this account cannot open: no workspace for the owner, or a repo its GitHub
 * token cannot read. It says nothing of the repo. */
export function NotHere({
  account,
  repo,
  number,
  login,
  onSignOut,
}: {
  account: string;
  repo: string;
  number: string;
  /** Who is signed in, when they have no workspace for `account`. */
  login?: string;
  onSignOut?: () => void;
}) {
  useEffect(() => sessionStorage.removeItem(AFTER_SIGN_IN), []);
  return (
    <div className="mx-auto max-w-md p-8 text-sm">
      <p className="font-medium">
        {login
          ? `No Mergepatrol workspace for ${account} on your account.`
          : `Mergepatrol has nothing to show you for ${account}/${repo}#${number}.`}
      </p>
      <p className="mt-2 text-muted">
        {login
          ? `You are signed in as ${login}. Sign in as a member of ${account} to see this PR in the merge queue.`
          : "Your GitHub account cannot read the repo, or the link is wrong."}
      </p>
      <div className="mt-4 flex gap-2">
        <a className={secondary} href={`https://github.com/${account}/${repo}/pull/${number}`}>
          Open on GitHub
        </a>
        {onSignOut && (
          <button type="button" className={secondary} onClick={onSignOut}>
            Sign out
          </button>
        )}
      </div>
    </div>
  );
}
