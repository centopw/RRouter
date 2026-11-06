import { defineConfig } from "vite";
import react from "@vitejs/plugin-react";
import tailwindcss from "@tailwindcss/vite";

// The dashboard is served by the gateway at /app. In dev, point the same paths at
// a running rrouter so the API is identical to production.
const target = process.env.RROUTER_URL ?? "http://127.0.0.1:20128";

export default defineConfig({
  base: "/app/",
  plugins: [react(), tailwindcss()],
  build: { outDir: "dist", emptyOutDir: true, sourcemap: false },
  server: {
    port: 5173,
    proxy: {
      "/api": target,
      "/v1": target,
      "/health": target,
      "/ext": target,
    },
  },
});
