import { createContext, useContext } from "react";
import type { AppView } from "./api/AppView";

/** The workspace in the URL, and the App it belongs to. The dashboard provides it to its pages. */
export const WorkspaceContext = createContext<{ workspace: number; app: AppView } | null>(null);

export function useWorkspace() {
  const value = useContext(WorkspaceContext);
  if (!value) throw new Error("useWorkspace outside the dashboard");
  return value;
}
