import { useMutation, useQueryClient } from "@tanstack/react-query";
import { type FormEvent, type ReactNode, useState } from "react";
import { startManifest, unlock } from "./api";
import type { AppView } from "./api/AppView";

function Card({ title, children }: { title: string; children: ReactNode }) {
  return (
    <section className="mx-auto mt-16 max-w-md rounded border border-neutral-800 p-6">
      <h1 className="mb-1 text-xl font-semibold">Mergepatrol</h1>
      <h2 className="mb-4 text-neutral-400">{title}</h2>
      {children}
    </section>
  );
}

const input = "w-full rounded border border-neutral-700 bg-neutral-900 px-3 py-1.5 text-sm";
const primary =
  "rounded bg-sky-700 px-3 py-1.5 text-sm hover:bg-sky-600 disabled:opacity-50 inline-block";

function ErrorText({ error }: { error: Error | null }) {
  return error ? <p className="mt-2 text-sm text-red-400">{error.message}</p> : null;
}

export function UnlockSetup() {
  const queryClient = useQueryClient();
  const [token, setToken] = useState("");
  const mutation = useMutation({
    mutationFn: unlock,
    onSuccess: () => queryClient.invalidateQueries({ queryKey: ["session"] }),
  });
  const submit = (e: FormEvent) => {
    e.preventDefault();
    mutation.mutate(token);
  };
  return (
    <Card title="Set up">
      <p className="mb-4 text-sm text-neutral-300">
        Enter the setup token. Mergepatrol prints it in its logs when it starts without a GitHub
        App: <code className="text-xs">docker compose logs mergepatrol | grep setup_token</code>
      </p>
      <form onSubmit={submit} className="space-y-3">
        <input
          className={`${input} font-mono`}
          placeholder="Setup token"
          autoComplete="off"
          value={token}
          onChange={(e) => setToken(e.target.value)}
        />
        <button type="submit" className={primary} disabled={!token.trim() || mutation.isPending}>
          Continue
        </button>
      </form>
      <ErrorText error={mutation.error} />
    </Card>
  );
}

/** Creates the GitHub App with GitHub's manifest flow: a form post from this browser. */
export function CreateApp() {
  const [owner, setOwner] = useState("");
  const [organization, setOrganization] = useState(true);
  const mutation = useMutation({
    mutationFn: startManifest,
    onSuccess: ({ action, manifest }) => {
      const form = document.createElement("form");
      form.method = "post";
      form.action = action;
      const field = document.createElement("input");
      field.type = "hidden";
      field.name = "manifest";
      field.value = manifest;
      form.appendChild(field);
      document.body.appendChild(form);
      form.submit();
    },
  });
  const submit = (e: FormEvent) => {
    e.preventDefault();
    mutation.mutate({ owner: owner.trim(), organization });
  };
  return (
    <Card title="Create the GitHub App">
      <p className="mb-4 text-sm text-neutral-300">
        Mergepatrol acts on GitHub as its own App. GitHub creates it for you, then asks where to
        install it.
      </p>
      <form onSubmit={submit} className="space-y-3">
        <div className="flex gap-4 text-sm">
          <label className="flex items-center gap-2">
            <input type="radio" checked={organization} onChange={() => setOrganization(true)} />{" "}
            Organization
          </label>
          <label className="flex items-center gap-2">
            <input type="radio" checked={!organization} onChange={() => setOrganization(false)} />{" "}
            My personal account
          </label>
        </div>
        <input
          className={input}
          placeholder={organization ? "Organization name, e.g. acme" : "Your GitHub username"}
          value={owner}
          onChange={(e) => setOwner(e.target.value)}
        />
        <button type="submit" className={primary} disabled={!owner.trim() || mutation.isPending}>
          Create on GitHub
        </button>
      </form>
      <ErrorText error={mutation.error} />
    </Card>
  );
}

export function SignIn() {
  return (
    <Card title="Sign in">
      <p className="mb-4 text-sm text-neutral-300">
        Mergepatrol rebases your pull requests and resolves conflicts with an agent.
      </p>
      <a className={primary} href="/auth/login">
        Sign in with GitHub
      </a>
    </Card>
  );
}

/** Signed in, but the App is on no account the user can access. */
export function Install({ app }: { app: AppView }) {
  return (
    <Card title="Install Mergepatrol">
      <p className="mb-4 text-sm text-neutral-300">
        Install the <code>{app.slug}</code> App on an organization or on your account, and choose
        its repos. Each installation is a workspace. GitHub sends you back here.
      </p>
      <a className={primary} href={app.install_url}>
        Install on GitHub
      </a>
    </Card>
  );
}
