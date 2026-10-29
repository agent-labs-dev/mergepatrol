import type { ThemeProps } from "./theme";
import { Mark, ThemeToggle } from "./ui";

/** Where the menu bar app sends the browser after its sign-in, which went to the app, not to
 * this browser. `error` is why it failed. */
export function TraySignedIn({ error, theme, setTheme }: { error: string | null } & ThemeProps) {
  return (
    <div className="flex min-h-screen flex-col">
      <header className="mx-auto flex h-14 w-full max-w-5xl items-center gap-2 px-6 font-semibold">
        <Mark /> Mergepatrol
        <div className="ml-auto font-normal">
          <ThemeToggle theme={theme} onChange={setTheme} />
        </div>
      </header>
      <main className="mx-auto grid max-w-md flex-1 place-items-center px-6 pb-24 text-center">
        <div>
          <img src="/logo.svg" alt="" className="mx-auto size-16 rounded-2xl shadow-sm" />
          {error ? (
            <>
              <h1 className="mt-8 text-2xl font-semibold tracking-tight">
                The menu bar app is not signed in
              </h1>
              <p className="mt-3 leading-relaxed text-fg-2 text-balance">{error}</p>
              <p className="mt-3 leading-relaxed text-fg-2">
                Try again from Mergepatrol in the menu bar.
              </p>
            </>
          ) : (
            <>
              <h1 className="mt-8 text-2xl font-semibold tracking-tight">
                The menu bar app is signed in
              </h1>
              <p className="mt-3 leading-relaxed text-fg-2 text-balance">
                Your merge queues are in the menu bar now. You can close this tab.
              </p>
            </>
          )}
        </div>
      </main>
    </div>
  );
}
