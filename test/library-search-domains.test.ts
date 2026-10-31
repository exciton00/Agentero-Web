import { beforeEach, describe, expect, it, vi } from "vitest";

const mocks = vi.hoisted(() => ({
	invoke: vi.fn(),
	hasHostTransport: vi.fn(() => true),
}));

vi.mock("@tauri-apps/api/core", () => ({
	invoke: mocks.invoke,
	Channel: class {},
}));
vi.mock("@/lib/core/tauri", () => ({
	hasHostTransport: mocks.hasHostTransport,
}));

import {
	matchesLibrarySearch,
	normalizePaperPath,
	searchPaperContent,
} from "@/lib/paper/search-content";
import {
	normalizeSearchDomains,
	PAPER_SEARCH_DOMAINS_STORAGE_KEY,
	readSearchDomains,
	writeSearchDomains,
} from "@/lib/paper/search-domains";

/** Minimal in-memory `Storage` for persistence tests. */
function memoryStorage(initial: Record<string, string> = {}) {
	const data = new Map(Object.entries(initial));
	return {
		getItem: (key: string) => data.get(key) ?? null,
		setItem: (key: string, value: string) => {
			data.set(key, value);
		},
		removeItem: (key: string) => {
			data.delete(key);
		},
		raw: data,
	};
}

describe("library search domains", () => {
	it("normalizes unknown, duplicate and out-of-order values", () => {
		expect(
			normalizeSearchDomains(["captions", "notes", "notes", "body"]),
		).toEqual(["notes", "captions"]);
		expect(normalizeSearchDomains("notes")).toEqual([]);
		expect(normalizeSearchDomains(null)).toEqual([]);
	});

	it("persists the selection and reads it back in canonical order", () => {
		const storage = memoryStorage();
		const domains = normalizeSearchDomains(["marks", "notes"]);
		expect(writeSearchDomains(domains, storage)).toBe(true);
		expect(storage.raw.get(PAPER_SEARCH_DOMAINS_STORAGE_KEY)).toBe(
			JSON.stringify(["notes", "marks"]),
		);
		expect(readSearchDomains(storage)).toEqual(["notes", "marks"]);
	});

	it("falls back to metadata-only for missing or corrupt storage", () => {
		expect(readSearchDomains(memoryStorage())).toEqual([]);
		expect(
			readSearchDomains(
				memoryStorage({ [PAPER_SEARCH_DOMAINS_STORAGE_KEY]: "{not json" }),
			),
		).toEqual([]);
		expect(
			readSearchDomains(
				memoryStorage({
					[PAPER_SEARCH_DOMAINS_STORAGE_KEY]: JSON.stringify(["body", 3]),
				}),
			),
		).toEqual([]);
	});

	it("tolerates storage that throws (private mode)", () => {
		const throwing = {
			getItem: () => {
				throw new Error("blocked");
			},
			setItem: () => {
				throw new Error("blocked");
			},
			removeItem: () => {},
		};
		expect(readSearchDomains(throwing)).toEqual([]);
		expect(writeSearchDomains(["notes"], throwing)).toBe(false);
	});
});

describe("library search row matching", () => {
	it("keeps default metadata matching when content domains are off", () => {
		expect(
			matchesLibrarySearch(
				"attention is all you need",
				"papers/a",
				"attention",
				null,
			),
		).toBe(true);
		expect(
			matchesLibrarySearch(
				"attention is all you need",
				"papers/a",
				"quantum",
				null,
			),
		).toBe(false);
		expect(matchesLibrarySearch("anything", "papers/a", "", null)).toBe(true);
	});

	it("unions content matches with metadata matches", () => {
		const content = new Set(["papers/a"]);
		expect(
			matchesLibrarySearch("unrelated", "papers/a", "notes-token", content),
		).toBe(true);
		expect(
			matchesLibrarySearch("unrelated", "papers/b", "notes-token", content),
		).toBe(false);
		// Metadata still wins even if the content set is stale/empty.
		expect(
			matchesLibrarySearch("notes-token", "papers/b", "notes-token", content),
		).toBe(true);
		expect(
			matchesLibrarySearch("unrelated", "papers/a", "notes-token", new Set()),
		).toBe(false);
	});

	it("normalizes Windows/leading-slash paper paths before set lookup", () => {
		expect(normalizePaperPath("papers\\nlp\\a")).toBe("papers/nlp/a");
		expect(normalizePaperPath("/papers/nlp/a/")).toBe("papers/nlp/a");
		const content = new Set(["papers/nlp/a"]);
		expect(
			matchesLibrarySearch("unrelated", "\\papers\\nlp\\a\\", "token", content),
		).toBe(true);
		expect(matchesLibrarySearch("unrelated", undefined, "token", content)).toBe(
			false,
		);
	});
});

describe("searchPaperContent", () => {
	beforeEach(() => {
		mocks.invoke.mockReset();
		mocks.hasHostTransport.mockReturnValue(true);
	});

	it("returns normalized matching paper paths from the Host envelope", async () => {
		mocks.invoke.mockResolvedValue({
			ok: true,
			data: {
				paperPaths: ["papers/a", "papers/b", "papers/a", 7, ""],
			},
		});
		const matches = await searchPaperContent({
			vaultPath: "/vault",
			query: "  quantum  ",
			domains: ["notes"],
		});
		expect([...matches]).toEqual(["papers/a", "papers/b"]);
		expect(mocks.invoke).toHaveBeenCalledWith("paper_search_content", {
			args: { vaultPath: "/vault", query: "quantum", domains: ["notes"] },
		});
	});

	it("stays off the wire when inactive", async () => {
		await searchPaperContent({
			vaultPath: "/vault",
			query: "   ",
			domains: ["notes"],
		});
		await searchPaperContent({
			vaultPath: "/vault",
			query: "quantum",
			domains: [],
		});
		mocks.hasHostTransport.mockReturnValue(false);
		await searchPaperContent({
			vaultPath: "/vault",
			query: "quantum",
			domains: ["notes"],
		});
		expect(mocks.invoke).not.toHaveBeenCalled();
	});

	it("degrades to no matches for hosts without the command", async () => {
		mocks.invoke.mockResolvedValue({ ok: false, data: null, error: null });
		await expect(
			searchPaperContent({
				vaultPath: "/vault",
				query: "quantum",
				domains: ["notes"],
			}),
		).resolves.toEqual(new Set());
	});

	it("surfaces Host failures so the caller can toast them", async () => {
		mocks.invoke.mockResolvedValue({
			ok: false,
			data: null,
			error: { message: "vault path is not a directory" },
		});
		await expect(
			searchPaperContent({
				vaultPath: "/missing",
				query: "quantum",
				domains: ["notes"],
			}),
		).rejects.toThrow("vault path is not a directory");
	});
});
