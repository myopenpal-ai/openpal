import { defineConfig } from "vitest/config";
import react from "@vitejs/plugin-react";
import tailwindcss from "@tailwindcss/vite";
import { fileURLToPath } from "node:url";

// Tauri loads this app from `ui/dist`; in dev it points at the Vite server.
export default defineConfig({
  plugins: [react(), tailwindcss()],
  // shadcn/ui components import `@/lib/utils`; tsconfig.json has the same alias.
  resolve: { alias: { "@": fileURLToPath(new URL("./src", import.meta.url)) } },
  clearScreen: false,
  server: {
    port: 5173,
    strictPort: true,
  },
  build: {
    // macOS WKWebView
    target: "safari15",
  },
  test: {
    environment: "jsdom",
    setupFiles: ["./src/test/setup.ts"],
    css: false,
    // Vitest's default include glob matches *.spec.ts too, which would
    // otherwise also try (and fail) to run the Playwright specs under
    // tests/e2e/ — those have their own runner (`npm run test:e2e`).
    include: ["src/**/*.{test,spec}.?(c|m)[jt]s?(x)"],
  },
});
