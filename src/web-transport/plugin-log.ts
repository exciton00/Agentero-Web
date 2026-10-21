/**
 * Browser replacement for `@tauri-apps/plugin-log`.
 *
 * The desktop plugin mirrors logs into the Host stdout + log directory; in the
 * browser the console is the only sink, so these helpers write there and the
 * app's own `logger` keeps working without a Host log file.
 */

type ConsoleMethod = "trace" | "debug" | "info" | "warn" | "error";

function write(level: ConsoleMethod, message: string): void {
	const fn =
		(console[level] as ((...args: unknown[]) => void) | undefined) ??
		console.log;
	fn.call(console, `[agentero] ${message}`);
}

export function trace(message: string): Promise<void> {
	write("trace", message);
	return Promise.resolve();
}

export function debug(message: string): Promise<void> {
	write("debug", message);
	return Promise.resolve();
}

export function info(message: string): Promise<void> {
	write("info", message);
	return Promise.resolve();
}

export function warn(message: string): Promise<void> {
	write("warn", message);
	return Promise.resolve();
}

export function error(message: string): Promise<void> {
	write("error", message);
	return Promise.resolve();
}

/** Dev-only stdout mirroring has no browser equivalent. */
export function attachConsole(): Promise<() => void> {
	return Promise.resolve(() => undefined);
}
