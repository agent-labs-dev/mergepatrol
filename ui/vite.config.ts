import tailwindcss from "@tailwindcss/vite";
import react from "@vitejs/plugin-react";
import { defineConfig } from "vite";
import { prototypeStatsMock } from "./prototype-stats-mock.ts";

export default defineConfig(({ mode }) => ({
  // PROTOTYPE: `pnpm prototype:stats` serves a fake API instead of proxying to the backend.
  plugins: [react(), tailwindcss(), ...(mode === "prototype" ? [prototypeStatsMock()] : [])],
  server: { proxy: { "/api": "http://127.0.0.1:8080" } },
}));
