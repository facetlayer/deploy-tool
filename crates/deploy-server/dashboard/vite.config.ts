import { defineConfig } from "vite";
import react from "@vitejs/plugin-react";

// The build is embedded in the deploy-server binary and served from the root
// of that server, so asset URLs are absolute — `serve_asset` in
// `src/dashboard/mod.rs` answers `/assets/*` out of this `dist` directory.
//
// In dev, run a local `deploy-server serve --port 4715` and everything the app
// calls is proxied to it, so the OAuth callback and the session cookie behave
// exactly as they do in production.
export default defineConfig({
  plugins: [react()],
  base: "/",
  build: { outDir: "dist", emptyOutDir: true },
  server: {
    proxy: {
      "/dashboard/api": "http://127.0.0.1:4715",
      "/oauth": "http://127.0.0.1:4715",
    },
  },
});
