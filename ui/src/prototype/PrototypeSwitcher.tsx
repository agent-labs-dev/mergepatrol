// PROTOTYPE: the floating variant switcher. Dev builds only.
import { useEffect } from "react";

export function PrototypeSwitcher({
  variants,
  current,
  onChange,
}: {
  variants: readonly (readonly [string, string])[];
  current: string;
  onChange: (key: string) => void;
}) {
  const i = Math.max(
    0,
    variants.findIndex(([k]) => k === current),
  );
  const go = (d: number) => onChange(variants[(i + d + variants.length) % variants.length][0]);
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
  if (!import.meta.env.DEV) return null;
  const [key, name] = variants[i];
  return (
    <div className="fixed bottom-5 left-1/2 z-50 flex -translate-x-1/2 items-center gap-1 rounded-full bg-fuchsia-600 p-1 text-sm text-white shadow-xl ring-4 ring-fuchsia-600/20">
      <button
        type="button"
        onClick={() => go(-1)}
        className="grid size-8 place-items-center rounded-full hover:bg-white/20"
      >
        ←
      </button>
      <span className="min-w-44 px-2 text-center font-medium">
        {key} <span className="opacity-80">({name})</span>
      </span>
      <button
        type="button"
        onClick={() => go(1)}
        className="grid size-8 place-items-center rounded-full hover:bg-white/20"
      >
        →
      </button>
    </div>
  );
}
