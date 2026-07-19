import react from "@vitejs/plugin-react";
import { defineConfig, searchForWorkspaceRoot } from "vite";

export default defineConfig({
  plugins: [react()],
  worker: { format: "es" },
  server: {
    fs: {
      // The example consumes the generated WASM package and browser host from
      // bindings/wasm without copying either artifact into the application.
      allow: [searchForWorkspaceRoot(process.cwd())],
    },
  },
});
