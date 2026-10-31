import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

/**
 * Watcher-driven Library refresh: a structural burst under `papers/` names the
 * touched paths, so the catalog repair must stay targeted (`paper_ensure_rows`)
 * instead of walking the whole `papers/` tree (`paper_rescan`). Path-less
 * lifecycle requests and hosts without the targeted command keep the rescan.
 */

const mocks = vi.hoisted(() => ({
	vaultPath: "/vault" as string | null,
	ensurePaperRows: vi.fn(),
	rescanPapers: vi.fn(async () => 0),
	listPapers: vi.fn(async () => []),
	setPaperTags: vi.fn(),
}));

vi.mock("@/lib/core/tauri", () => ({ hasHostTransport: () => true }));
vi.mock("@/lib/vault/store", () => ({ getVaultPath: () => mocks.vaultPath }));
vi.mock("@/lib/paper/api", () => ({
	ensurePaperRows: mocks.ensurePaperRows,
	rescanPapers: mocks.rescanPapers,
	listPapers: mocks.listPapers,
	setPaperTags: mocks.setPaperTags,
}));

import {
	clearLibraryVaultState,
	scheduleLibraryRefresh,
} from "@/lib/paper/library-store";

const flushRefresh = () => vi.advanceTimersByTimeAsync(1_000);

describe("targeted Library refresh after watcher bursts", () => {
	beforeEach(() => {
		vi.useFakeTimers();
		mocks.vaultPath = "/vault";
		mocks.ensurePaperRows.mockResolvedValue({ count: 1, rescanned: false });
		mocks.rescanPapers.mockResolvedValue(0);
		mocks.listPapers.mockResolvedValue([]);
	});

	afterEach(() => {
		clearLibraryVaultState();
		vi.clearAllMocks();
		vi.useRealTimers();
	});

	it("repairs only the watcher-touched paper folders", async () => {
		scheduleLibraryRefresh([
			"/vault/papers/a/NOTES.md",
			"/vault/papers/a/metadata.json",
		]);
		await flushRefresh();

		expect(mocks.ensurePaperRows).toHaveBeenCalledTimes(1);
		expect(mocks.ensurePaperRows).toHaveBeenCalledWith("/vault", [
			"/vault/papers/a/NOTES.md",
			"/vault/papers/a/metadata.json",
		]);
		expect(mocks.rescanPapers).not.toHaveBeenCalled();
		expect(mocks.listPapers).toHaveBeenCalledWith("/vault");
	});

	it("merges repeated bursts into one targeted repair", async () => {
		scheduleLibraryRefresh(["/vault/papers/a/NOTES.md"]);
		scheduleLibraryRefresh(["/vault/papers/b/NOTES.md"]);
		await flushRefresh();

		expect(mocks.ensurePaperRows).toHaveBeenCalledTimes(1);
		expect(mocks.ensurePaperRows).toHaveBeenCalledWith("/vault", [
			"/vault/papers/a/NOTES.md",
			"/vault/papers/b/NOTES.md",
		]);
	});

	it("falls back to the full rescan when the host lacks the targeted command", async () => {
		mocks.ensurePaperRows.mockResolvedValue(null);
		scheduleLibraryRefresh(["/vault/papers/a/NOTES.md"]);
		await flushRefresh();

		expect(mocks.rescanPapers).toHaveBeenCalledTimes(1);
		expect(mocks.listPapers).toHaveBeenCalledTimes(1);
	});

	it("keeps a path-less request on the full rescan", async () => {
		scheduleLibraryRefresh();
		await flushRefresh();

		expect(mocks.ensurePaperRows).not.toHaveBeenCalled();
		expect(mocks.rescanPapers).toHaveBeenCalledTimes(1);
	});

	it("upgrades a queued targeted refresh when a path-less request arrives", async () => {
		scheduleLibraryRefresh(["/vault/papers/a/NOTES.md"]);
		scheduleLibraryRefresh();
		await flushRefresh();

		expect(mocks.ensurePaperRows).not.toHaveBeenCalled();
		expect(mocks.rescanPapers).toHaveBeenCalledTimes(1);
	});

	it("does nothing when no vault is open", async () => {
		mocks.vaultPath = null;
		scheduleLibraryRefresh(["/vault/papers/a/NOTES.md"]);
		await flushRefresh();

		expect(mocks.ensurePaperRows).not.toHaveBeenCalled();
		expect(mocks.rescanPapers).not.toHaveBeenCalled();
	});
});
