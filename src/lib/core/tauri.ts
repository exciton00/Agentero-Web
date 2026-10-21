/** True when running inside a Tauri webview (not plain browser). */
export function isTauri(): boolean {
	if (typeof window === "undefined") return false;
	// The headless web client installs a `__TAURI_INTERNALS__` compatibility
	// object for code probing the Tauri globals; it is still not a Tauri shell.
	if (isWebHost()) return false;
	return "__TAURI_INTERNALS__" in window || "__TAURI__" in window;
}

/**
 * True when this frontend bundle runs as the headless web client.
 *
 * `vite --mode web` (dev or build) is the only bundle that ships the HTTP/WS
 * transport shims from `src/web-transport/`; the desktop build keeps its
 * default mode and never sets this.
 */
export function isWebHost(): boolean {
	return import.meta.env.MODE === "web";
}

/**
 * True when a Host transport is available to this frontend.
 *
 * Both the Tauri desktop shell and the headless web host speak the same Host
 * command/event contract — only the transport differs (IPC vs HTTP + WS).
 * Guard Host calls with this; keep {@link isTauri} for genuinely native
 * capabilities (updater, native windows, OS file dialogs, CLI sidecars).
 */
export function hasHostTransport(): boolean {
	return isWebHost() || isTauri();
}

/**
 * Best-effort desktop OS detection from the webview UA.
 * Mirrors the platform checks in `shortcuts.ts` / `reveal.ts`.
 */
export function getPlatformOS(): "macos" | "windows" | "linux" | "other" {
	if (typeof navigator === "undefined") return "other";
	const platform = navigator.platform ?? "";
	const ua = navigator.userAgent ?? "";
	if (/Mac|iPhone|iPad|iPod/.test(platform)) return "macos";
	if (/Win/.test(platform) || /Windows/.test(ua)) return "windows";
	if (/Linux|X11/.test(platform)) return "linux";
	return "other";
}

/**
 * True for WebKit webviews (macOS WKWebView, Linux WebKitGTK).
 *
 * Blink (WebView2 / Chrome / Edge) and Gecko both include an `AppleWebKit`
 * token in their UA, so the token alone is not enough: only treat a UA as
 * WebKit when it has no Chromium tokens. Used to gate rendering optimizations
 * whose cost/benefit differs by engine.
 */
export function isWebKitEngine(ua?: string): boolean {
	const userAgent =
		ua ?? (typeof navigator !== "undefined" ? navigator.userAgent : "");
	if (!/AppleWebKit/.test(userAgent)) return false;
	return !/(Chrome|Chromium|CriOS|Edg|OPR)\//.test(userAgent);
}

/**
 * macOS keeps native traffic lights via the Overlay title bar; other desktop
 * platforms use a frameless window with custom caption buttons.
 */
export function isMacOS(): boolean {
	return getPlatformOS() === "macos";
}

/** Windows desktop shell (user-PATH registry handling, terminal wording). */
export function isWindows(): boolean {
	return getPlatformOS() === "windows";
}

/** Phone/tablet app shell: iPhone/iPad (incl. iPadOS `MacIntel`) or Android. */
export function isMobileApp(): boolean {
	if (typeof window === "undefined" || typeof navigator === "undefined") {
		return false;
	}
	if (
		new URLSearchParams(window.location.search).get("mobilePreview") === "1"
	) {
		return true;
	}
	const platform = navigator.platform ?? "";
	const ua = navigator.userAgent ?? "";
	return (
		/iPhone|iPad|iPod/.test(platform) ||
		/iPhone|iPad|iPod|Android/.test(ua) ||
		(platform === "MacIntel" && navigator.maxTouchPoints > 1)
	);
}
