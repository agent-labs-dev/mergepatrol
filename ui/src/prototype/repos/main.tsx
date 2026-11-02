// PROTOTYPE: Repos page layouts, four variants switchable via `?variant=A|B|C|D`, on fixture data
// so it runs without the server: `pnpm prototype:repos`. Throwaway; never merge to main.
import { StrictMode } from "react";
import { createRoot } from "react-dom/client";
import "../../index.css";
import { useTheme } from "../../theme";
import { Mark, ThemeToggle } from "../../ui";
import { PrototypeSwitcher, useVariant } from "../PrototypeSwitcher";
import { useRepos } from "./data";
import { VariantA, VariantB, VariantC, VariantD } from "./variants";

const VARIANTS = [
  { key: "A", name: "Dense table", C: VariantA },
  { key: "B", name: "Needs attention", C: VariantB },
  { key: "C", name: "List + detail", C: VariantC },
  { key: "D", name: "Filter + bulk", C: VariantD },
];

function Prototype() {
  const [variant, setVariant] = useVariant(VARIANTS.map((v) => v.key));
  const [theme, setTheme] = useTheme();
  const store = useRepos();
  const V = VARIANTS.find((v) => v.key === variant)?.C ?? VariantA;
  return (
    <div className="flex h-dvh flex-col">
      {/* The dashboard's header, copied flat so the variants sit in the real chrome. */}
      <header className="border-b border-line bg-surface">
        <div className="flex h-12 items-center gap-3 px-5">
          <Mark />
          <span className="text-faint">/</span>
          <span className="font-medium">agent-labs-dev</span>
          <div className="ml-auto flex items-center gap-3">
            <span className="text-muted">Feedback</span>
            <ThemeToggle theme={theme} onChange={setTheme} />
          </div>
        </div>
        <nav className="flex gap-5 px-5">
          {["You", "Queue", "History", "Repos", "Settings"].map((t) => (
            <span
              key={t}
              className={`-mb-px border-b-2 pb-2.5 ${t === "Repos" ? "border-fg text-fg" : "border-transparent text-muted"}`}
            >
              {t}
            </span>
          ))}
        </nav>
      </header>
      <main className="min-h-0 flex-1 overflow-y-auto pb-24">
        <V key={variant} store={store} />
      </main>
      <PrototypeSwitcher variants={VARIANTS} current={variant} onChange={setVariant} />
    </div>
  );
}

const root = document.getElementById("root");
if (!root) throw new Error("no #root");
createRoot(root).render(
  <StrictMode>
    <Prototype />
  </StrictMode>,
);
