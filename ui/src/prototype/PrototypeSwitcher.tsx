// PROTOTYPE: the floating variant switcher. Never mount it outside src/prototype.
import { useEffect, useState } from "react";

export function useVariant(keys: string[]) {
  const read = () => {
    const v = new URLSearchParams(location.search).get("variant");
    return v && keys.includes(v) ? v : keys[0];
  };
  const [variant, set] = useState(read);
  const go = (v: string) => {
    const url = new URL(location.href);
    url.searchParams.set("variant", v);
    history.replaceState(null, "", url);
    set(v);
  };
  return [variant, go] as const;
}

export function PrototypeSwitcher({
  variants,
  current,
  onChange,
}: {
  variants: { key: string; name: string }[];
  current: string;
  onChange: (key: string) => void;
}) {
  const i = variants.findIndex((v) => v.key === current);
  const step = (d: number) => onChange(variants[(i + d + variants.length) % variants.length].key);
  useEffect(() => {
    const onKey = (e: KeyboardEvent) => {
      const t = e.target as HTMLElement;
      if (t.closest("input, textarea, select, [contenteditable]")) return;
      if (e.key === "ArrowLeft") step(-1);
      if (e.key === "ArrowRight") step(1);
    };
    addEventListener("keydown", onKey);
    return () => removeEventListener("keydown", onKey);
  });
  if (import.meta.env.PROD) return null;
  return (
    <div className="fixed bottom-4 left-1/2 z-50 flex -translate-x-1/2 items-center gap-1 rounded-full bg-fuchsia-600 p-1 text-white shadow-xl ring-4 ring-fuchsia-600/20">
      <button type="button" onClick={() => step(-1)} className="rounded-full px-3 py-1.5 hover:bg-white/15">
        ←
      </button>
      {variants.map((v) => (
        <button
          key={v.key}
          type="button"
          onClick={() => onChange(v.key)}
          className={`rounded-full px-3 py-1.5 text-xs font-medium ${v.key === current ? "bg-white text-fuchsia-700" : "hover:bg-white/15"}`}
        >
          {v.key} · {v.name}
        </button>
      ))}
      <button type="button" onClick={() => step(1)} className="rounded-full px-3 py-1.5 hover:bg-white/15">
        →
      </button>
    </div>
  );
}
