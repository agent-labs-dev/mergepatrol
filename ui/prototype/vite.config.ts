// PROTOTYPE, throw away. `pnpm prototype`: the dashboard against the fake API in `mock-api.ts`.
import tailwindcss from "@tailwindcss/vite";
import react from "@vitejs/plugin-react";
import { defineConfig } from "vite";
import { mockApi } from "./mock-api.ts";

export default defineConfig({
  root: `${import.meta.dirname}/..`,
  plugins: [mockApi(), react(), tailwindcss()],
});
