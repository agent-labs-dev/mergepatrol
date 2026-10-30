import { Toast } from "@base-ui/react/toast";
import { useEffect, useState } from "react";
import { primary } from "./ui";

type Action = { label: string; onClick: () => void; primary?: boolean };

/** What a toast carries past its title and description. `ms` is how long a `held` toast waits. */
export type ToastData = { actions?: Action[]; ms?: number };

/** Every toast the dashboard shows, which any module may add to. A toast's `type` picks its icon:
 * `success`, `held` (counting down to something it will do), `loading`, or `update`. */
export const toasts = Toast.createToastManager<ToastData>();

/** The stack of toasts, bottom right. It fans out on hover, and a toast swipes away down or
 * right. */
export function Toaster() {
  return (
    <Toast.Provider toastManager={toasts} timeout={4000}>
      <Toast.Portal>
        <Toast.Viewport className="fixed right-4 bottom-4 z-50 w-96 max-w-[calc(100vw-2rem)]">
          <List />
        </Toast.Viewport>
      </Toast.Portal>
    </Toast.Provider>
  );
}

function List() {
  return Toast.useToastManager<ToastData>().toasts.map((t) => (
    <Toast.Root
      key={t.id}
      toast={t}
      className="toast rounded-xl border border-line bg-surface text-sm shadow-lg dark:shadow-black/40"
    >
      <Toast.Content className="toast-content flex gap-3 p-4">
        <Icon type={t.type} ms={t.data?.ms} />
        <div className="min-w-0 flex-1">
          <Toast.Title className="font-medium text-fg" />
          <Toast.Description render={<div />} className="mt-0.5 text-muted" />
          {t.data?.actions && (
            <div className="mt-3 flex justify-end gap-1">
              {t.data.actions.map((a) => (
                <button
                  key={a.label}
                  type="button"
                  onClick={a.onClick}
                  className={
                    a.primary
                      ? primary
                      : "h-9 rounded-md px-3 text-fg-2 hover:bg-sunken hover:text-fg"
                  }
                >
                  {a.label}
                </button>
              ))}
            </div>
          )}
        </div>
        <Toast.Close
          aria-label="Dismiss"
          className="-mt-1 -mr-1 grid size-6 shrink-0 place-items-center self-start rounded-md text-faint hover:bg-sunken hover:text-fg"
        >
          <svg viewBox="0 0 16 16" className="size-3.5" aria-hidden>
            <path
              d="m4 4 8 8m0-8-8 8"
              stroke="currentColor"
              strokeWidth="1.5"
              strokeLinecap="round"
            />
          </svg>
        </Toast.Close>
      </Toast.Content>
    </Toast.Root>
  ));
}

function Icon({ type, ms }: { type?: string; ms?: number }) {
  const ring = "size-5 shrink-0";
  switch (type) {
    case "success":
      return (
        <svg viewBox="0 0 20 20" className={`${ring} text-ok`} aria-hidden>
          <circle cx="10" cy="10" r="9" fill="currentColor" />
          <path
            d="m6.5 10.2 2.3 2.3 4.7-4.9"
            fill="none"
            stroke="white"
            strokeWidth="1.75"
            strokeLinecap="round"
            strokeLinejoin="round"
          />
        </svg>
      );
    case "held":
      // Drains over `ms`, as the time to undo runs out.
      return (
        <svg viewBox="0 0 20 20" className={`${ring} -rotate-90 text-fg`} aria-hidden>
          <circle cx="10" cy="10" r="8" fill="none" stroke="var(--line)" strokeWidth="2.5" />
          <circle
            cx="10"
            cy="10"
            r="8"
            fill="none"
            stroke="currentColor"
            strokeWidth="2.5"
            pathLength="1"
            strokeDasharray="1"
            className="toast-countdown"
            style={{ animationDuration: `${ms ?? 0}ms` }}
          />
        </svg>
      );
    case "loading":
      return (
        <svg viewBox="0 0 20 20" className={`${ring} animate-spin text-fg`} aria-hidden>
          <circle cx="10" cy="10" r="8" fill="none" stroke="var(--line)" strokeWidth="2.5" />
          <path
            d="M10 2a8 8 0 0 1 8 8"
            fill="none"
            stroke="currentColor"
            strokeWidth="2.5"
            strokeLinecap="round"
          />
        </svg>
      );
    case "update":
      return (
        <svg viewBox="0 0 20 20" className={`${ring} text-[var(--step-1)]`} aria-hidden>
          <circle cx="10" cy="10" r="9" fill="currentColor" />
          <path
            d="M10 14.5v-9m-3.5 3.5L10 5.5 13.5 9"
            fill="none"
            stroke="white"
            strokeWidth="1.75"
            strokeLinecap="round"
            strokeLinejoin="round"
          />
        </svg>
      );
    default:
      return null;
  }
}

/** "Sends in 3s", ticking down to `until`. */
export function Countdown({ until, verb }: { until: number; verb: string }) {
  const [now, setNow] = useState(Date.now());
  useEffect(() => {
    const t = setInterval(() => setNow(Date.now()), 250);
    return () => clearInterval(t);
  }, []);
  return (
    <>
      {verb} in {Math.max(1, Math.ceil((until - now) / 1000))}s
    </>
  );
}
