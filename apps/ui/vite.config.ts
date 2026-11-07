import { defineConfig } from "vite";
import react from "@vitejs/plugin-react";
import { viteSingleFile } from "vite-plugin-singlefile";

// One self-contained index.html (scripts, styles and the font inlined): the apps embed it in their
// binaries and serve it from their own URL scheme, so the UI never touches the network.
export default defineConfig({
  plugins: [react(), viteSingleFile()],
  build: { target: "es2022", assetsInlineLimit: 100_000_000 },
});
