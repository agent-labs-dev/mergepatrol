/* PROTOTYPE, throw away. Cycles `?variant=` with the arrows or ← →. Dev builds only. */
import { useEffect } from "react";

export function PrototypeSwitcher<K extends string>({
  variants,
  current,
  onChange,
}: {
  variants: Record<K, string>;
  current: K;
  onChange: (k: K) => void;
}) {
  const keys = Object.keys(variants) as K[];
  const step = (d: number) =>
    onChange(keys[(keys.indexOf(current) + d + keys.length) % keys.length]);
  useEffect(() => {
    const on = (e: KeyboardEvent) => {
      if ((e.target as HTMLElement).closest("input,textarea,select,[contenteditable]")) return;
      if (e.key === "ArrowLeft") step(-1);
      if (e.key === "ArrowRight") step(1);
    };
    window.addEventListener("keydown", on);
    return () => window.removeEventListener("keydown", on);
  });
  if (!import.meta.env.DEV) return null;
  const arrow = "grid size-7 place-items-center rounded-full hover:bg-white/15";
  return (
    <div className="fixed bottom-5 left-1/2 z-50 flex -translate-x-1/2 items-center gap-1 rounded-full bg-fuchsia-600 px-1.5 py-1 text-sm text-white shadow-lg ring-2 ring-white/40">
      <button
        type="button"
        onClick={() => step(-1)}
        className={arrow}
        aria-label="Previous variant"
      >
        ←
      </button>
      <span className="px-2 font-medium whitespace-nowrap">
        {current} <span className="font-normal opacity-80">({variants[current]})</span>
      </span>
      <button type="button" onClick={() => step(1)} className={arrow} aria-label="Next variant">
        →
      </button>
    </div>
  );
}
