/**
 * Clipboard helpers — single place for writeText + optional success/error toasts.
 */

import {
	type NotifyOptions,
	notifyError,
	notifySuccess,
} from "@/lib/core/notify";

export type CopyTextOptions = {
	/** Success toast; omit for silent success. */
	successMessage?: string;
	/** Error toast; omit for silent failure (still returns false). */
	errorMessage?: string;
	/** Extra options for the success toast (duration, id, …). */
	successNotify?: NotifyOptions;
};

export type ReadTextOptions = {
	/** Error toast; omit for silent failure (still returns null). */
	errorMessage?: string;
};

/**
 * Copy through a temporary `<textarea>` + `document.execCommand("copy")`.
 *
 * `navigator.clipboard` is only available in secure contexts (HTTPS or
 * localhost). A self-hosted Agentero-Web instance is often opened over plain
 * HTTP on a LAN/NAS address, where the async Clipboard API is missing — PDF
 * selection copy would otherwise always report "Could not copy the selection".
 * `execCommand` still works there when invoked from a user gesture.
 */
function copyWithExecCommand(text: string): boolean {
	if (typeof document === "undefined" || typeof document.body === "undefined") {
		return false;
	}
	const textarea = document.createElement("textarea");
	textarea.value = text;
	textarea.setAttribute("readonly", "");
	textarea.style.position = "fixed";
	textarea.style.top = "-1000px";
	textarea.style.left = "-1000px";
	textarea.style.opacity = "0";
	document.body.appendChild(textarea);
	const previousRange =
		typeof document.getSelection === "function" &&
		document.getSelection()?.rangeCount
			? document.getSelection()!.getRangeAt(0).cloneRange()
			: null;
	let copied = false;
	try {
		textarea.select();
		textarea.setSelectionRange(0, text.length);
		copied = document.execCommand("copy");
	} catch {
		copied = false;
	} finally {
		document.body.removeChild(textarea);
		if (previousRange) {
			const selection = document.getSelection();
			selection?.removeAllRanges();
			selection?.addRange(previousRange);
		}
	}
	return copied;
}

/**
 * Write plain text to the system clipboard.
 *
 * Prefers the async Clipboard API and falls back to `execCommand` for
 * insecure (plain HTTP) self-hosted origins.
 * @returns true when the write succeeded.
 */
export async function copyTextToClipboard(
	text: string,
	opts: CopyTextOptions = {},
): Promise<boolean> {
	try {
		if (typeof navigator !== "undefined" && navigator.clipboard?.writeText) {
			try {
				await navigator.clipboard.writeText(text);
				if (opts.successMessage) {
					notifySuccess(opts.successMessage, opts.successNotify);
				}
				return true;
			} catch {
				// Permission denied / insecure context: try the legacy fallback.
			}
		}
		if (!copyWithExecCommand(text)) {
			throw new Error("clipboard unavailable");
		}
		if (opts.successMessage) {
			notifySuccess(opts.successMessage, opts.successNotify);
		}
		return true;
	} catch {
		if (opts.errorMessage) {
			notifyError(opts.errorMessage);
		}
		return false;
	}
}

/**
 * Read plain text from the system clipboard.
 * @returns clipboard text, including an empty string, or null when unavailable.
 */
export async function readTextFromClipboard(
	opts: ReadTextOptions = {},
): Promise<string | null> {
	try {
		if (typeof navigator === "undefined" || !navigator.clipboard?.readText) {
			throw new Error("clipboard unavailable");
		}
		return await navigator.clipboard.readText();
	} catch {
		if (opts.errorMessage) {
			notifyError(opts.errorMessage);
		}
		return null;
	}
}
