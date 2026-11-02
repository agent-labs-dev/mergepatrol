import { useMutation, useQueryClient } from "@tanstack/react-query";
import { type FormEvent, type ReactNode, useState } from "react";
import { startManifest, unlock } from "./api";
import type { AppView } from "./api/AppView";
import { ErrorText, field, Mark, primary } from "./ui";

/** The first run's steps on a self-hosted MergeLoop, which shows its progress. */
const setupSteps = 2;

function Card({ step, title, children }: { step?: number; title: string; children: ReactNode }) {
  return (
    <div className="flex min-h-screen flex-col items-center justify-center px-4">
      <div className="mb-6 flex items-center gap-2 font-semibold">
        <Mark /> MergeLoop
      </div>
      <section className="w-full max-w-md rounded-xl border border-line bg-surface p-6 shadow-sm sm:p-8">
        {step && (
          <ol className="mb-6 flex gap-1.5" aria-label={`Step ${step} of ${setupSteps}`}>
            {Array.from({ length: setupSteps }, (_, i) => (
              <li
                // biome-ignore lint/suspicious/noArrayIndexKey: a fixed list of steps
                key={i}
                className={`h-1 flex-1 rounded-full ${i < step ? "bg-fg" : "bg-sunken"}`}
              />
            ))}
          </ol>
        )}
        <h1 className="text-xl font-semibold tracking-tight">{title}</h1>
        {children}
      </section>
    </div>
  );
}

function Body({ children }: { children: ReactNode }) {
  return <p className="mt-2 leading-relaxed text-fg-2">{children}</p>;
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
    <Card step={1} title="Set up MergeLoop">
      <Body>
        Enter the setup token. MergeLoop prints it in its logs when it starts without a GitHub App:
      </Body>
      <pre className="mt-3 overflow-x-auto rounded-md bg-sunken px-3 py-2 font-mono text-xs">
        docker compose logs mergepatrol | grep setup_token
      </pre>
      <form onSubmit={submit} className="mt-5 space-y-3">
        <input
          className={`${field} w-full font-mono`}
          placeholder="Setup token"
          autoComplete="off"
          value={token}
          onChange={(e) => setToken(e.target.value)}
        />
        <button
          type="submit"
          className={`${primary} w-full`}
          disabled={!token.trim() || mutation.isPending}
        >
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
      const input = document.createElement("input");
      input.type = "hidden";
      input.name = "manifest";
      input.value = manifest;
      form.appendChild(input);
      document.body.appendChild(form);
      form.submit();
    },
  });
  const submit = (e: FormEvent) => {
    e.preventDefault();
    mutation.mutate({ owner: owner.trim(), organization });
  };
  const choice = (on: boolean, label: string) => (
    <button
      type="button"
      onClick={() => setOrganization(on)}
      className={`h-8 flex-1 rounded ${organization === on ? "bg-surface shadow-xs ring-1 ring-line" : "text-muted hover:text-fg"}`}
    >
      {label}
    </button>
  );
  return (
    <Card step={2} title="Create the GitHub App">
      <Body>
        MergeLoop acts on GitHub as its own App. GitHub creates it for you, then asks where to
        install it.
      </Body>
      <form onSubmit={submit} className="mt-5 space-y-3">
        <div className="flex gap-1 rounded-md bg-sunken p-1">
          {choice(true, "Organization")}
          {choice(false, "My personal account")}
        </div>
        <input
          className={`${field} w-full`}
          placeholder={organization ? "Organization name, e.g. acme" : "Your GitHub username"}
          value={owner}
          onChange={(e) => setOwner(e.target.value)}
        />
        <button
          type="submit"
          className={`${primary} w-full`}
          disabled={!owner.trim() || mutation.isPending}
        >
          Create on GitHub
        </button>
      </form>
      <ErrorText error={mutation.error} />
    </Card>
  );
}

/** Signed in, but the App is on no account the user can access. */
export function Install({ app }: { app: AppView }) {
  return (
    <Card title="Install the App">
      <Body>
        Install <code className="rounded bg-sunken px-1 text-sm">{app.slug}</code> on an
        organization or on your account, and choose its repos. Each installation is a workspace.
        GitHub sends you back here.
      </Body>
      <a className={`${primary} mt-5 w-full`} href={app.install_url}>
        Install on GitHub
      </a>
    </Card>
  );
}
