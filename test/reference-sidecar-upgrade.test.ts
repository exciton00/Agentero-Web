import { beforeEach, describe, expect, it, vi } from "vitest";

const mocks = vi.hoisted(() => ({
	paperRefsList: vi.fn(),
	jobParseRefsEnqueue: vi.fn(),
}));

vi.mock("@/lib/core/bindings", () => ({
	commands: {
		paperRefsList: mocks.paperRefsList,
		jobParseRefsEnqueue: mocks.jobParseRefsEnqueue,
	},
}));
vi.mock("@/lib/core/ipc", () => ({
	callApi: async (fn: () => Promise<{ ok: boolean; data?: unknown }>) => {
		const result = await fn();
		if (!result.ok) throw new Error("host command failed");
		return result.data;
	},
	callApiResult: async (fn: () => Promise<unknown>) => fn(),
}));

import { CITE_SCHEMA_VERSION, loadPaperRefsReadOnly } from "@/lib/paper/refs";

function sidecar(schemaVersion: number) {
	return {
		schemaVersion,
		source: { mode: "paper_md", generatedAt: "t", fingerprint: "f" },
		citations: [],
		messages: [],
	};
}

/** Let the fire-and-forget enqueue promise settle. */
const flush = () => new Promise((resolve) => setTimeout(resolve, 0));

describe("reference sidecar upgrade", () => {
	beforeEach(() => {
		mocks.paperRefsList.mockReset();
		mocks.jobParseRefsEnqueue.mockReset();
		mocks.jobParseRefsEnqueue.mockResolvedValue({
			status: "ok",
			data: { ok: true, data: null },
		});
	});

	it("does not reparse a current sidecar", async () => {
		mocks.paperRefsList.mockResolvedValue({
			ok: true,
			data: sidecar(CITE_SCHEMA_VERSION),
		});
		const loaded = await loadPaperRefsReadOnly("/vault", "papers/a");
		await flush();
		expect(loaded?.schemaVersion).toBe(CITE_SCHEMA_VERSION);
		expect(mocks.jobParseRefsEnqueue).not.toHaveBeenCalled();
	});

	it("backfills a missing sidecar in the background", async () => {
		mocks.paperRefsList.mockResolvedValue({ ok: true, data: null });
		const loaded = await loadPaperRefsReadOnly("/vault", "papers/missing");
		await flush();
		expect(loaded).toBeNull();
		expect(mocks.jobParseRefsEnqueue).toHaveBeenCalledTimes(1);
		expect(mocks.jobParseRefsEnqueue).toHaveBeenCalledWith({
			vaultPath: "/vault",
			path: "papers/missing",
			force: false,
		});
	});

	it("reparses a sidecar written by the older parser", async () => {
		mocks.paperRefsList.mockResolvedValue({
			ok: true,
			data: sidecar(CITE_SCHEMA_VERSION - 1),
		});
		const loaded = await loadPaperRefsReadOnly("/vault", "papers/legacy");
		await flush();
		// The old sidecar is returned immediately; the panel shows it until the
		// background parse settles and emits job:changed.
		expect(loaded?.schemaVersion).toBe(CITE_SCHEMA_VERSION - 1);
		expect(mocks.jobParseRefsEnqueue).toHaveBeenCalledTimes(1);
	});

	it("dedupes concurrent backfill requests for the same paper", async () => {
		mocks.paperRefsList.mockResolvedValue({ ok: true, data: null });
		let release: () => void = () => {};
		mocks.jobParseRefsEnqueue.mockImplementation(
			() =>
				new Promise((resolve) => {
					release = () =>
						resolve({ status: "ok", data: { ok: true, data: null } });
				}),
		);

		await Promise.all([
			loadPaperRefsReadOnly("/vault", "papers/same"),
			loadPaperRefsReadOnly("/vault", "papers/same"),
		]);
		await flush();
		expect(mocks.jobParseRefsEnqueue).toHaveBeenCalledTimes(1);
		release();
		await flush();
	});

	it("keeps the old display schema constant in sync with the Host", () => {
		// Mirrors `refs::SCHEMA_VERSION` in agentero-core.
		expect(CITE_SCHEMA_VERSION).toBe(2);
	});
});
