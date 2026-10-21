/**
 * `Channel<T>` shim for the generated specta bindings (`@tauri-apps/api/core`).
 *
 * Desktop Tauri routes channel messages through the IPC callback table. The
 * headless host has no channel transport yet (docs/development/webui.md: the
 * two Zotero migration/sync commands are desktop-only), so the shim preserves
 * the class shape the generated bindings expect — `new Channel<T>()`,
 * `channel.onmessage = …`, `toJSON()` → `__CHANNEL__:<id>` — and drops
 * messages. Commands that take a Channel currently answer a normal business
 * error, which callers surface as a toast.
 */

let nextChannelId = 1;

export class Channel<T = unknown> {
	/** Callback id sent to the backend inside the serialized `toJSON` string. */
	readonly id: number;

	private handler: (message: T) => void;

	constructor(onmessage?: (message: T) => void) {
		this.id = nextChannelId;
		nextChannelId += 1;
		this.handler = onmessage ?? (() => undefined);
	}

	get onmessage(): (message: T) => void {
		return this.handler;
	}

	set onmessage(handler: (message: T) => void) {
		this.handler = handler;
	}

	/** Wire form understood by the desktop Host (`__CHANNEL__:<callback id>`). */
	toJSON(): string {
		return `__CHANNEL__:${this.id}`;
	}
}
