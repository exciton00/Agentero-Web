import { beforeEach, describe, expect, it, vi } from "vitest";
import {
	applyCachedHistoryTitles,
	getCachedHistoryTitle,
	getOverriddenHistoryTitle,
	setCachedHistoryTitle,
	setUserHistoryTitle,
} from "@/lib/agent/history-title-cache";

function memoryStorage() {
	const map = new Map<string, string>();
	return {
		getItem: (key: string) => map.get(key) ?? null,
		setItem: (key: string, value: string) => {
			map.set(key, value);
		},
		removeItem: (key: string) => {
			map.delete(key);
		},
	};
}

describe("history title cache overrides", () => {
	beforeEach(() => {
		vi.stubGlobal("localStorage", memoryStorage());
	});

	it("stores user renames as overrides that always win (#710)", () => {
		setCachedHistoryTitle("agent-1", "s1", "Hydrated");
		setUserHistoryTitle("agent-1", "s1", "Renamed", "s1");
		expect(getOverriddenHistoryTitle("agent-1", "s1")).toBe("Renamed");

		const applied = applyCachedHistoryTitles("agent-1", [
			{ id: "s1", title: "ACP title", providerSessionId: "s1" },
			{ id: "s2", title: "", providerSessionId: "s2" },
		]);
		expect(applied[0]).toMatchObject({
			title: "Renamed",
			titleLocked: true,
		});
		// Non-overridden empty titles still pick up the regular cache when present.
		setCachedHistoryTitle("agent-1", "s2", "From cache");
		const again = applyCachedHistoryTitles("agent-1", [
			{ id: "s2", title: "", providerSessionId: "s2" },
		]);
		expect(again[0].title).toBe("From cache");
		expect(again[0].titleLocked).toBeUndefined();
	});
});

describe("history title cache — machine fallback titles", () => {
	beforeEach(() => {
		vi.stubGlobal("localStorage", memoryStorage());
	});

	it("ignores stale machine titles written by earlier builds", () => {
		setCachedHistoryTitle("agent-1", "s1", "You are an assistant working");
		expect(getCachedHistoryTitle("agent-1", "s1")).toBeNull();

		const applied = applyCachedHistoryTitles("agent-1", [
			{ id: "s1", title: "", providerSessionId: "s1" },
		]);
		expect(applied[0].title).toBe("");
	});

	it("never stores a machine title as a hydrated label", () => {
		setCachedHistoryTitle("agent-1", "s2", "You are running the Agentero");
		expect(getCachedHistoryTitle("agent-1", "s2")).toBeNull();
		setCachedHistoryTitle("agent-1", "s2", "real question");
		expect(getCachedHistoryTitle("agent-1", "s2")).toBe("real question");
	});
});
