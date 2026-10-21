/// <reference types="vitest" />

import { readFileSync } from "node:fs";
import path from "node:path";
import tailwindcss from "@tailwindcss/vite";
import react from "@vitejs/plugin-react";
import { defineConfig } from "vite";

// @ts-expect-error process is a nodejs global
const host = process.env.TAURI_DEV_HOST;
// @ts-expect-error process is a nodejs global
const port = Number(process.env.TAURI_DEV_PORT) || 1420;
// @ts-expect-error process is a nodejs global
const webPort = Number(process.env.AGENTERO_WEB_DEV_PORT) || 5173;
// @ts-expect-error process is a nodejs global
const webApiTarget = process.env.AGENTERO_WEB_PROXY || "http://127.0.0.1:8787";

/** `package.json` version, embedded into the web build for Settings → About. */
const appVersion = (
	JSON.parse(
		readFileSync(path.resolve(process.cwd(), "package.json"), "utf8"),
	) as { version?: string }
).version as string;

/**
 * Web-mode transport aliases.
 *
 * The React app imports the Tauri JS packages directly; in a browser those
 * packages need a real Tauri IPC bridge. For `--mode web` every `@tauri-apps/*`
 * import is redirected to the browser shim in `src/web-transport/` that speaks
 * HTTP + WebSocket to the headless host. The desktop build (default mode) does
 * not get these aliases and keeps using the real packages unchanged.
 */
function webTransportAliases(): Array<{
	find: RegExp;
	replacement: string;
}> {
	const shimDir = path.resolve(__dirname, "./src/web-transport");
	const shim = (file: string) => path.join(shimDir, file);
	return [
		{ find: /^@tauri-apps\/api\/core$/, replacement: shim("core.ts") },
		{ find: /^@tauri-apps\/api\/event$/, replacement: shim("event.ts") },
		{ find: /^@tauri-apps\/api\/path$/, replacement: shim("path.ts") },
		{ find: /^@tauri-apps\/api\/app$/, replacement: shim("app.ts") },
		{ find: /^@tauri-apps\/api\/window$/, replacement: shim("window.ts") },
		{ find: /^@tauri-apps\/api\/webview$/, replacement: shim("webview.ts") },
		{
			find: /^@tauri-apps\/api\/webviewWindow$/,
			replacement: shim("webviewWindow.ts"),
		},
		// `@tauri-apps/plugin-fs|dialog|opener|log|updater|process|deep-link`
		// → `src/web-transport/plugin-<name>.ts`.
		{
			find: /^@tauri-apps\/plugin-([a-z0-9-]+)$/,
			replacement: `${shimDir}/plugin-$1.ts`,
		},
	];
}

// https://vite.dev/config/
export default defineConfig(async ({ mode }) => {
	const isWeb = mode === "web";
	return {
		plugins: [react(), tailwindcss()],

		// Web build only: the About pane reads the package version.
		define: isWeb ? { __APP_VERSION__: JSON.stringify(appVersion) } : undefined,

		resolve: {
			alias: [
				{ find: /^@\//, replacement: `${path.resolve(__dirname, "./src")}/` },
				...(isWeb ? webTransportAliases() : []),
			],
		},

		// EmbedPDF ships a PDFium WASM binary + web worker. Emit the wasm as an
		// asset, bundle the worker as an ES module, and keep Vite from pre-bundling
		// these packages (dep-optimize rewrites break their wasm/worker loading).
		assetsInclude: ["**/*.wasm"],
		worker: {
			format: "es",
		},
		optimizeDeps: {
			// PDFium + ONNX Runtime load wasm/workers; dep-optimize rewrites break them.
			exclude: [
				"@embedpdf/pdfium",
				"@embedpdf/engines",
				"onnxruntime-web",
				"@embedpdf/ai",
				// Do NOT add `@platejs/toc` here even though it is patched in-repo.
				// It imports the CommonJS-only `react-compiler-runtime`
				// (`import { c } from "react-compiler-runtime"`), and only the
				// prebundle step performs the CJS→ESM interop that resolves `c`.
				// Excluding it fails in the webview with
				// "Importing binding name 'c' is not found".
				//
				// The patch still reaches `vite dev` because pnpm resolves the
				// package to its `patch_hash=…` store dir, and the prebundle cache
				// key covers the lockfile — so a patch edit invalidates it.
			],
		},

		test: {
			environment: "node",
			include: ["test/**/*.test.ts"],
			server: {
				deps: {
					inline: [/@embedpdf\/ai/],
				},
			},
		},

		// Vite options tailored for Tauri development and only applied in `tauri dev` or `tauri build`
		//
		// 1. prevent Vite from obscuring rust errors
		clearScreen: false,
		// 2. tauri expects a fixed port, fail if that port is not available
		server: isWeb
			? {
					// `dev:web`: Vite serves the SPA, the headless host serves the API.
					// The proxy keeps the browser on one origin (no CORS) and forwards
					// the `/api/events` WebSocket too.
					port: webPort,
					strictPort: true,
					host: "127.0.0.1",
					proxy: {
						"/api": {
							target: webApiTarget,
							changeOrigin: false,
							ws: true,
						},
					},
				}
			: {
					port,
					strictPort: true,
					// iOS Simulator reaches the Mac dev server through the local
					// network bridge; binding only to loopback makes
					// localhost:1420 unreachable.
					host: host || "0.0.0.0",
					hmr: host
						? {
								protocol: "ws",
								host,
								port: port + 1,
							}
						: undefined,
					watch: {
						// 3. tell Vite to ignore watching `src-tauri`
						ignored: ["**/src-tauri/**"],
					},
				},
	};
});
