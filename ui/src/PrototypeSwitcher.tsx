// PROTOTYPE, delete me: flips between UI variants via `?variant=`, ← and →.
import { useEffect, useState } from "react";

// Tab links drop unknown search params, so the choice also lives in localStorage.
const read = (keys: string[]) => {
  const v = new URLSearchParams(location.search).get("variant") ?? localStorage.getItem("variant");
  return v && keys.includes(v) ? v : keys[0];
};

export function useVariant(variants: Record<string, string>) {
  const keys = Object.keys(variants);
  const [current, setCurrent] = useState(() => read(keys));
  const go = (step: number) => {
    const next = keys[(keys.indexOf(current) + step + keys.length) % keys.length];
    const url = new URL(location.href);
    url.searchParams.set("variant", next);
    history.replaceState(history.state, "", url);
    localStorage.setItem("variant", next);
    setCurrent(next);
  };
  useEffect(() => {
    const onKey = (e: KeyboardEvent) => {
      const t = e.target as HTMLElement;
      if (t.closest("input, textarea, select, [contenteditable]")) return;
      if (e.key === "ArrowLeft") go(-1);
      if (e.key === "ArrowRight") go(1);
    };
    addEventListener("keydown", onKey);
    return () => removeEventListener("keydown", onKey);
  });
  const bar = import.meta.env.DEV ? (
    <div className="fixed bottom-4 left-1/2 z-50 flex -translate-x-1/2 items-center gap-1 rounded-full bg-black px-2 py-1.5 text-sm text-white shadow-2xl ring-1 ring-white/20">
      <button type="button" onClick={() => go(-1)} className="rounded-full px-2 hover:bg-white/15">
        ←
      </button>
      <span className="min-w-48 text-center font-medium">
        {current} ({variants[current]})
      </span>
      <button type="button" onClick={() => go(1)} className="rounded-full px-2 hover:bg-white/15">
        →
      </button>
    </div>
  ) : null;
  return [current, bar] as const;
}
