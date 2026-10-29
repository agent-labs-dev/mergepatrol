import { parsePatchFiles } from "@pierre/diffs";
import { FileDiff } from "@pierre/diffs/react";
import { type CSSProperties, useMemo, useSyncExternalStore } from "react";
import type { FixView } from "./api/FixView";

/** A fix's files, each as a diff. Its own chunk: the highlighter is most of the dashboard's
 * weight, and only an opened fix needs it. */
export default function FixDiff({ fix }: { fix: FixView }) {
  const files = useMemo(() => parsePatchFiles(fix.patch).flatMap((p) => p.files), [fix.patch]);
  const dark = useDark();
  return (
    <div className="space-y-2">
      {files.map((f) => (
        <FileDiff
          key={f.name}
          fileDiff={f}
          className="block overflow-hidden rounded-md border border-line"
          style={{ "--diffs-font-size": "12px" } as CSSProperties}
          options={{
            diffStyle: "unified",
            themeType: dark ? "dark" : "light",
            overflow: "wrap",
            hunkSeparators: "line-info",
            lineDiffType: "word",
          }}
        />
      ))}
      {fix.omitted.length > 0 && (
        <p className="text-faint">No diff from GitHub for {fix.omitted.join(", ")}.</p>
      )}
    </div>
  );
}

/** Whether the dashboard is dark, as `useTheme` sets it on the root. */
const useDark = () =>
  useSyncExternalStore(
    (onChange) => {
      const o = new MutationObserver(onChange);
      o.observe(document.documentElement, { attributes: true, attributeFilter: ["class"] });
      return () => o.disconnect();
    },
    () => document.documentElement.classList.contains("dark"),
  );
