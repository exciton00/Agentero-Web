/**
 * Browser implementation of `@tauri-apps/api/event` over the web host
 * WebSocket (`GET /api/events?label=<tab>`).
 *
 * The desktop Host pushes events through the Tauri IPC; the headless host
 * pushes the same frames over one WS per browser tab (docs/development/
 * webui.md §2.2/§2.3). Frames look like `{ event, target?, payload }`, where
 * `target` is the window label a targeted `emit_to` was aimed at:
 * - no `target` → broadcast, every tab delivers it;
 * - `target === this tab's label` → deliver;
 * - otherwise → drop (the server fans every frame out to every connection in
 *   v1, so the per-tab filtering happens here).
 *
 * The listener table is kept in memory across reconnects: the server has no
 * per-event subscriptions to restore, it just needs a live socket again.
 */

import { getWindowLabel } from "./label";

/** Removes a registered listener. */
export type UnlistenFn = () => void;

/** Wire event handed to a listener; matches the Tauri `Event<T>` shape. */
export type Event<T> = {
	event: string;
	id: number;
	payload: T;
};

export type EventCallback<T> = (event: Event<T>) => void;

/** Options accepted by Tauri's `listen`/`once` (target is implicit here). */
export type EventOptions = {
	target?: string | { label?: string } | null;
};

/** Options accepted by Tauri's `emit`; `target` narrows delivery to one tab. */
export type EmitOptions = {
	target?: string | { label?: string } | null;
};

type Listener = {
	event: string;
	callback: EventCallback<unknown>;
	once: boolean;
};

const listeners = new Set<Listener>();
const pendingEmits: string[] = [];
let socket: WebSocket | null = null;
let reconnectTimer: number | null = null;
let reconnectAttempt = 0;
let nextEventId = 1;

const RECONNECT_BASE_MS = 500;
const RECONNECT_MAX_MS = 10_000;
const MAX_PENDING_EMITS = 100;

function isRecord(value: unknown): value is Record<string, unknown> {
	return typeof value === "object" && value !== null;
}

function socketUrl(): string {
	const scheme = window.location.protocol === "https:" ? "wss" : "ws";
	const label = encodeURIComponent(getWindowLabel());
	return `${scheme}://${window.location.host}/api/events?label=${label}`;
}

function deliver(frame: Record<string, unknown>): void {
	const event = typeof frame.event === "string" ? frame.event : null;
	// `$hello` (and future `$…` frames) are transport-internal handshakes.
	if (!event || event.startsWith("$")) return;
	const target = typeof frame.target === "string" ? frame.target : null;
	if (target && target !== getWindowLabel()) return;
	const payload = frame.payload;
	for (const listener of [...listeners]) {
		if (listener.event !== event) continue;
		if (listener.once) listeners.delete(listener);
		try {
			listener.callback({ event, id: nextEventId, payload });
			nextEventId += 1;
		} catch (error) {
			console.error(`[web-transport] listener for "${event}" threw`, error);
		}
	}
}

function scheduleReconnect(): void {
	if (reconnectTimer !== null || typeof window === "undefined") return;
	const delay = Math.min(
		RECONNECT_BASE_MS * 2 ** reconnectAttempt,
		RECONNECT_MAX_MS,
	);
	reconnectAttempt += 1;
	reconnectTimer = window.setTimeout(() => {
		reconnectTimer = null;
		ensureSocket();
	}, delay);
}

/** Open the shared event socket (no-op while open or connecting). */
export function ensureSocket(): void {
	if (typeof window === "undefined" || typeof WebSocket === "undefined") {
		return;
	}
	if (
		socket &&
		(socket.readyState === WebSocket.OPEN ||
			socket.readyState === WebSocket.CONNECTING)
	) {
		return;
	}
	let next: WebSocket;
	try {
		next = new WebSocket(socketUrl());
	} catch (error) {
		console.warn("[web-transport] cannot open event socket", error);
		scheduleReconnect();
		return;
	}
	socket = next;
	next.addEventListener("open", () => {
		reconnectAttempt = 0;
		// The server wires `label` from the query string; sending `hello` keeps
		// the documented client → server handshake explicit.
		try {
			next.send(
				JSON.stringify({
					type: "hello",
					windowLabel: getWindowLabel(),
				}),
			);
		} catch {
			// Socket closed between open and send; `close` schedules a retry.
		}
		flushPendingEmits();
	});
	next.addEventListener("message", (message) => {
		if (typeof message.data !== "string") return;
		try {
			const frame = JSON.parse(message.data) as unknown;
			if (isRecord(frame)) deliver(frame);
		} catch {
			// Ignore malformed frames instead of killing the connection.
		}
	});
	next.addEventListener("close", () => {
		if (socket === next) socket = null;
		scheduleReconnect();
	});
	next.addEventListener("error", () => {
		// `close` follows an error; it schedules the reconnect.
		try {
			next.close();
		} catch {
			// Already closed.
		}
	});
}

function send(text: string): void {
	ensureSocket();
	if (socket && socket.readyState === WebSocket.OPEN) {
		socket.send(text);
		return;
	}
	if (pendingEmits.length >= MAX_PENDING_EMITS) pendingEmits.shift();
	pendingEmits.push(text);
}

function flushPendingEmits(): void {
	if (!socket || socket.readyState !== WebSocket.OPEN) return;
	for (const text of pendingEmits.splice(0)) {
		try {
			socket.send(text);
		} catch {
			// Connection dropped mid-flush; remaining frames are re-queued by
			// the next `send` call after reconnect.
		}
	}
}

function resolveTarget(options?: EmitOptions): string | null {
	const target = options?.target;
	if (!target) return null;
	if (typeof target === "string") return target;
	return typeof target.label === "string" ? target.label : null;
}

function subscribe(
	event: string,
	callback: EventCallback<unknown>,
	once: boolean,
): Promise<UnlistenFn> {
	const listener: Listener = { event, callback, once };
	listeners.add(listener);
	ensureSocket();
	return Promise.resolve(() => {
		listeners.delete(listener);
	});
}

/**
 * Listen for a Host event. The `options.target` of the Tauri API is accepted
 * for signature parity: every socket already belongs to this tab, and frames
 * targeted at other tabs are filtered in {@link deliver}.
 */
export function listen<T>(
	event: string,
	handler: EventCallback<T>,
	_options?: EventOptions,
): Promise<UnlistenFn> {
	return subscribe(event, handler as EventCallback<unknown>, false);
}

/** Listen for a single occurrence of a Host event. */
export function once<T>(
	event: string,
	handler: EventCallback<T>,
	_options?: EventOptions,
): Promise<UnlistenFn> {
	return subscribe(event, handler as EventCallback<unknown>, true);
}

/**
 * Emit an event to the Host. Without `options.target` the server broadcasts it
 * to every tab (matching Tauri's global `emit`).
 */
export function emit<T>(
	event: string,
	payload?: T,
	options?: EmitOptions,
): Promise<void> {
	const target = resolveTarget(options);
	send(
		JSON.stringify({ type: "emit", event, payload: payload ?? null, target }),
	);
	return Promise.resolve();
}
