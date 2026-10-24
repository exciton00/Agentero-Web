import { afterEach, describe, expect, it, vi } from "vitest";
import { randomUuid } from "@/lib/core/uuid";

const originalCrypto = globalThis.crypto;

afterEach(() => {
	vi.unstubAllGlobals();
	Object.defineProperty(globalThis, "crypto", {
		configurable: true,
		value: originalCrypto,
	});
});

describe("randomUuid", () => {
	it("returns RFC 4122 v4 UUIDs", () => {
		const value = randomUuid();
		expect(value).toMatch(
			/^[0-9a-f]{8}-[0-9a-f]{4}-4[0-9a-f]{3}-[89ab][0-9a-f]{3}-[0-9a-f]{12}$/,
		);
		expect(randomUuid()).not.toBe(value);
	});

	it("falls back when crypto.randomUUID is unavailable (plain HTTP)", () => {
		const getRandomValues = vi.fn((bytes: Uint8Array) => {
			bytes.set(Array.from({ length: 16 }, (_, i) => i + 1));
			return bytes;
		});
		vi.stubGlobal("crypto", { getRandomValues });
		expect(randomUuid()).toBe("01020304-0506-4708-890a-0b0c0d0e0f10");
		expect(getRandomValues).toHaveBeenCalledOnce();
	});

	it("works without any Web Crypto implementation", () => {
		vi.stubGlobal("crypto", undefined);
		expect(randomUuid()).toMatch(
			/^[0-9a-f]{8}-[0-9a-f]{4}-4[0-9a-f]{3}-[89ab][0-9a-f]{3}-[0-9a-f]{12}$/,
		);
	});
});
