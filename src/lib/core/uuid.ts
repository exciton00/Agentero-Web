/**
 * UUID v4 that also works on plain-HTTP origins.
 *
 * `crypto.randomUUID()` is only exposed in secure contexts (HTTPS or
 * localhost). A self-hosted Web UI reached over `http://<LAN-IP>:8787` is not
 * a secure context, so annotation creation used to reject with
 * `TypeError: crypto.randomUUID is not a function`. `crypto.getRandomValues`
 * is available there; fall back to it (and finally Math.random only for
 * environments with no Web Crypto at all).
 */
export function randomUuid(): string {
	const cryptoObj =
		typeof globalThis.crypto === "object" ? globalThis.crypto : null;

	if (cryptoObj && typeof cryptoObj.randomUUID === "function") {
		try {
			return cryptoObj.randomUUID();
		} catch {
			// Some embedded/webview crypto shims expose the method but reject.
		}
	}

	const bytes = new Uint8Array(16);
	if (cryptoObj && typeof cryptoObj.getRandomValues === "function") {
		cryptoObj.getRandomValues(bytes);
	} else {
		for (let i = 0; i < bytes.length; i += 1) {
			bytes[i] = Math.floor(Math.random() * 256);
		}
	}

	// RFC 4122 version 4 / variant bits.
	bytes[6] = (bytes[6] & 0x0f) | 0x40;
	bytes[8] = (bytes[8] & 0x3f) | 0x80;

	const hex: string[] = [];
	for (const byte of bytes) hex.push(byte.toString(16).padStart(2, "0"));
	return [
		hex.slice(0, 4).join(""),
		hex.slice(4, 6).join(""),
		hex.slice(6, 8).join(""),
		hex.slice(8, 10).join(""),
		hex.slice(10, 16).join(""),
	].join("-");
}
