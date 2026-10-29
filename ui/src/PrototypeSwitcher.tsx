// PROTOTYPE, throwaway: flips between a page's `?variant=`s. ← and → cycle too.
import { useNavigate } from "@tanstack/react-router";
import { useEffect } from "react";

export function PrototypeSwitcher({
  variants,
  current,
}: {
  variants: [key: string, name: string][];
  current: string;
}) {
  const navigate = useNavigate();
  const i = Math.max(
    0,
    variants.findIndex(([k]) => k === current),
  );
  const go = (d: number) => {
    const [variant] = variants[(i + d + variants.length) % variants.length];
    // biome-ignore lint/suspicious/noExplicitAny: prototype
    navigate({ to: ".", search: (s: any) => ({ ...s, variant }), replace: true } as any);
  };
  useEffect(() => {
    const onKey = (e: KeyboardEvent) => {
      const t = e.target as HTMLElement;
      if (t.closest("input, textarea, select, [contenteditable]")) return;
      if (e.key === "ArrowLeft") go(-1);
      if (e.key === "ArrowRight") go(1);
    };
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
  });
  if (import.meta.env.PROD) return null;
  const [key, name] = variants[i];
  return (
    <div className="fixed bottom-4 left-1/2 z-50 flex -translate-x-1/2 items-center gap-1 rounded-full bg-fuchsia-600 px-1 py-1 text-sm font-medium text-white shadow-lg ring-2 ring-white/40">
      <button
        type="button"
        className="rounded-full px-3 py-1 hover:bg-white/20"
        onClick={() => go(-1)}
      >
        ←
      </button>
      <span className="min-w-56 text-center tabular-nums">
        {key} ({name})
      </span>
      <button
        type="button"
        className="rounded-full px-3 py-1 hover:bg-white/20"
        onClick={() => go(1)}
      >
        →
      </button>
    </div>
  );
}
