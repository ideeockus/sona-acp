import { svelte } from "@sveltejs/vite-plugin-svelte";
import { defineConfig, loadEnv } from "vite";

// Locally the dev server stands in for the reverse proxy: one origin, /v1 to the
// control plane (D98). The Host header stays as the browser sent it, so the API's
// Origin check sees the same site. SONA_API points elsewhere than the default.
export default defineConfig(({ mode }) => {
  const env = loadEnv(mode, ".", "SONA_");
  return {
    plugins: [svelte()],
    server: {
      proxy: {
        "/v1": { target: env.SONA_API ?? "http://127.0.0.1:7700", changeOrigin: false },
      },
    },
    build: { target: "es2022" },
  };
});
