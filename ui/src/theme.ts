import { useEffect, useState } from "react";

export const themes = ["light", "dark", "system"] as const;
export type Theme = (typeof themes)[number];
export type ThemeProps = { theme: Theme; setTheme: (t: Theme) => void };

const media = matchMedia("(prefers-color-scheme: dark)");

const stored = (): Theme => {
  const t = localStorage.getItem("theme");
  return t === "light" || t === "dark" ? t : "system";
};

/** The chosen theme, kept in localStorage. `system` follows the OS, live. */
export function useTheme() {
  const [theme, setTheme] = useState(stored);
  useEffect(() => {
    const apply = () =>
      document.documentElement.classList.toggle(
        "dark",
        theme === "dark" || (theme === "system" && media.matches),
      );
    apply();
    if (theme === "system") localStorage.removeItem("theme");
    else localStorage.setItem("theme", theme);
    media.addEventListener("change", apply);
    return () => media.removeEventListener("change", apply);
  }, [theme]);
  return [theme, setTheme] as const;
}
