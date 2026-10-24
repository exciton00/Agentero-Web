import { afterEach, describe, expect, it, vi } from "vitest";
import { copyTextToClipboard } from "@/lib/core/clipboard";

afterEach(() => {
	vi.unstubAllGlobals();
});

describe("copyTextToClipboard on a plain-HTTP origin", () => {
	it("falls back to execCommand when navigator.clipboard is unavailable", async () => {
		const execCommand = vi.fn(() => true);
		const textarea = {
			value: "",
			style: {} as CSSStyleDeclaration,
			setAttribute: vi.fn(),
			select: vi.fn(),
			setSelectionRange: vi.fn(),
		};
		const documentStub = {
			body: {
				appendChild: vi.fn(),
				removeChild: vi.fn(),
			},
			createElement: vi.fn(() => textarea),
			getSelection: vi.fn(() => null),
			execCommand,
		};
		vi.stubGlobal("navigator", {});
		vi.stubGlobal("document", documentStub);

		await expect(copyTextToClipboard("selected PDF text")).resolves.toBe(true);
		expect(documentStub.createElement).toHaveBeenCalledWith("textarea");
		expect(execCommand).toHaveBeenCalledWith("copy");
		expect(textarea.value).toBe("selected PDF text");
	});

	it("uses the async Clipboard API when it is available", async () => {
		const writeText = vi.fn(async () => undefined);
		vi.stubGlobal("navigator", { clipboard: { writeText } });
		vi.stubGlobal("document", {});

		await expect(copyTextToClipboard("text")).resolves.toBe(true);
		expect(writeText).toHaveBeenCalledWith("text");
	});
});
