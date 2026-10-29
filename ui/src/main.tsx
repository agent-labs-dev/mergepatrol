import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import { RouterProvider } from "@tanstack/react-router";
import { StrictMode } from "react";
import { createRoot } from "react-dom/client";
import { router } from "./router";
import "./index.css";
import { installMockApi } from "./prototype-fix-diff/mock";
import { prototypeOn } from "./prototype-fix-diff/variants";

// PROTOTYPE, throw away: the fix-diff prototype runs on a fake API.
if (prototypeOn()) installMockApi();

const root = document.getElementById("root");
if (!root) throw new Error("no #root");

createRoot(root).render(
  <StrictMode>
    <QueryClientProvider client={new QueryClient()}>
      <RouterProvider router={router} />
    </QueryClientProvider>
  </StrictMode>,
);
