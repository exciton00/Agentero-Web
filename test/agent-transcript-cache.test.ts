import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import type { AgentPart, ChatLine } from "@/lib/agent/chat-state";
import {
	clearCachedTranscripts,
	createTranscriptPersister,
	discardScheduledTranscriptSaves,
	findCachedTranscriptByUserTurns,
	flushScheduledTranscriptSaves,
	hasVisibleAssistantText,
	loadCachedTranscript,
	MAX_CACHE_ENTRIES,
	MAX_CACHED_LINES,
	MAX_LINE_CHARS,
	MAX_SESSION_CHARS,
	mergeTranscriptLines,
	removeCachedTranscript,
	resolveCachedTranscript,
	sanitizeTranscriptLinesForCache,
	saveCachedTranscript,
	scheduleTranscriptSave,
	TRANSCRIPT_CACHE_STORAGE_KEY,
	TRANSCRIPT_SAVE_DEBOUNCE_MS,
	transcriptCacheKey,
	transcriptText,
} from "@/lib/agent/transcript-cache";

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

function memoryStorage(options?: { quota?: number }) {
	const map = new Map<string, string>();
	const quota = options?.quota ?? Number.POSITIVE_INFINITY;
	return {
		getItem: (key: string) => map.get(key) ?? null,
		setItem: (key: string, value: string) => {
			if (value.length > quota) throw new Error("quota exceeded");
			map.set(key, value);
		},
		removeItem: (key: string) => {
			map.delete(key);
		},
	};
}

function userLine(id: string, text: string): ChatLine {
	return { id, kind: "user", text };
}

function agentLine(id: string, parts: AgentPart[]): ChatLine {
	return { id, kind: "agent", parts };
}

function textPart(text: string, id = "t1"): AgentPart {
	return { type: "text", id, text };
}

function reasoningPart(text: string, id = "r1"): AgentPart {
	return { type: "reasoning", id, text };
}

function toolPart(
	id: string,
	status: "pending" | "in_progress" | "completed" | "failed",
	extra?: { input?: unknown; output?: unknown },
): AgentPart {
	return {
		type: "tool",
		id: `part-${id}`,
		tool: {
			id,
			title: "read",
			kind: "read",
			status,
			...extra,
		},
	};
}

/** Simulate one interrupted turn as Dsh persists it: reasoning/tool, no text. */
function interruptedTurn(): ChatLine[] {
	return [
		userLine("u1", "Explain the paper"),
		agentLine("a1", [
			reasoningPart("thinking"),
			toolPart("tool-1", "in_progress"),
		]),
	];
}

beforeEach(() => {
	vi.stubGlobal("localStorage", memoryStorage());
	discardScheduledTranscriptSaves();
});

afterEach(() => {
	discardScheduledTranscriptSaves();
	vi.useRealTimers();
	vi.unstubAllGlobals();
});

// ---------------------------------------------------------------------------
// Cache representation / sanitize
// ---------------------------------------------------------------------------

describe("sanitizeTranscriptLinesForCache", () => {
	it("strips composer images, visual crops and selection chips from user turns", () => {
		const base64 = "A".repeat(12_000);
		const lines = [
			{
				id: "u1",
				kind: "user" as const,
				text: "look at this",
				images: [{ data: base64, mimeType: "image/png" }],
				visualAnnotations: [
					{
						id: "v1",
						page: 3,
						comment: "crop",
						image: { data: base64, mimeType: "image/png" },
					},
				],
				selections: [
					{
						id: "s1",
						text: "selected words",
						sourcePath: "papers/x/PAPER.md",
						origin: "pdf" as const,
						pinned: true,
					},
				],
			},
		];

		const sanitized = sanitizeTranscriptLinesForCache(lines);

		expect(sanitized).toEqual([
			{ id: "u1", kind: "user", text: "look at this" },
		]);
		expect(JSON.stringify(sanitized)).not.toContain("AAAAAAAA");
		expect(JSON.stringify(sanitized)).not.toContain("selected words");
	});

	it("keeps small tool payloads but drops huge ones (screenshots / base64)", () => {
		const lines = [
			agentLine("a1", [
				toolPart("small", "completed", { input: { cmd: "ls" }, output: "ok" }),
				toolPart("huge", "completed", {
					output: { image: "B".repeat(9_000) },
				}),
			]),
		];

		const sanitized = sanitizeTranscriptLinesForCache(lines);
		const agent = sanitized[0];
		expect(agent?.kind).toBe("agent");
		if (agent?.kind !== "agent") return;
		const small = agent.parts.find(
			(part) => part.type === "tool" && part.tool.id === "small",
		);
		const huge = agent.parts.find(
			(part) => part.type === "tool" && part.tool.id === "huge",
		);
		expect(small?.type === "tool" ? small.tool.input : undefined).toEqual({
			cmd: "ls",
		});
		expect(
			huge?.type === "tool" ? huge.tool.output : "missing",
		).toBeUndefined();
	});

	it("clamps oversized text and drops streaming flags", () => {
		const lines = [
			userLine("u1", "x".repeat(MAX_LINE_CHARS + 5_000)),
			{ ...agentLine("a1", [textPart("answer")]), streaming: true } as ChatLine,
		];

		const sanitized = sanitizeTranscriptLinesForCache(lines);
		const user = sanitized[0];
		expect(user?.kind === "user" ? user.text.length : 0).toBe(MAX_LINE_CHARS);
		const agent = sanitized[1];
		expect(agent?.kind === "agent" ? agent.streaming : true).toBe(false);
	});

	it("keeps only the newest turn-aligned window of lines", () => {
		const lines: ChatLine[] = [];
		for (let index = 0; index < MAX_CACHED_LINES * 1.5; index++) {
			lines.push(
				index % 2 === 0
					? userLine(`u-${index}`, `question ${index}`)
					: agentLine(`a-${index}`, [textPart(`answer ${index}`)]),
			);
		}

		const sanitized = sanitizeTranscriptLinesForCache(lines);

		expect(sanitized.length).toBeLessThanOrEqual(MAX_CACHED_LINES);
		expect(sanitized[0]?.kind).toBe("user");
		expect(transcriptText(sanitized)).toContain("answer 149");
	});

	it("drops invalid lines instead of storing half a transcript", () => {
		const sanitized = sanitizeTranscriptLinesForCache([
			{ id: "bad", kind: "bogus" } as unknown as ChatLine,
			userLine("u1", "kept"),
		]);
		expect(sanitized).toEqual([{ id: "u1", kind: "user", text: "kept" }]);
	});
});

// ---------------------------------------------------------------------------
// Storage roundtrip / tolerance / capacity
// ---------------------------------------------------------------------------

describe("transcript cache storage", () => {
	it("roundtrips a transcript under agent + session key", () => {
		const lines = [
			userLine("u1", "question"),
			agentLine("a1", [reasoningPart("think"), textPart("final answer")]),
		];

		expect(saveCachedTranscript("dsh", "session-1", lines)).toBe(true);

		const loaded = loadCachedTranscript("dsh", ["session-1"]);
		expect(loaded).not.toBeNull();
		expect(transcriptText(loaded ?? [])).toContain("final answer");
		expect(loadCachedTranscript("dsh", ["session-2"])).toBeNull();
		expect(loadCachedTranscript("other-agent", ["session-1"])).toBeNull();
	});

	it("ignores corrupt JSON, wrong versions and malformed entries", () => {
		localStorage.setItem(TRANSCRIPT_CACHE_STORAGE_KEY, "{not json");
		expect(loadCachedTranscript("dsh", ["s1"])).toBeNull();

		localStorage.setItem(
			TRANSCRIPT_CACHE_STORAGE_KEY,
			JSON.stringify({ v: 99, entries: {} }),
		);
		expect(loadCachedTranscript("dsh", ["s1"])).toBeNull();

		// A hand-edited entry keeps only valid lines.
		localStorage.setItem(
			TRANSCRIPT_CACHE_STORAGE_KEY,
			JSON.stringify({
				v: 1,
				entries: {
					[transcriptCacheKey("dsh", "s1")]: {
						at: Date.now(),
						lines: [
							null,
							{ kind: "nope" },
							{ id: "u1", kind: "user", text: "hi" },
						],
					},
				},
			}),
		);
		expect(loadCachedTranscript("dsh", ["s1"])).toEqual([
			{ id: "u1", kind: "user", text: "hi" },
		]);
	});

	it("clamps a session transcript to its size budget", () => {
		const lines: ChatLine[] = [];
		for (let index = 0; index < 30; index++) {
			lines.push(userLine(`u-${index}`, `question ${index}`));
			lines.push(agentLine(`a-${index}`, [textPart("y".repeat(60_000))]));
		}

		saveCachedTranscript("dsh", "big", lines);

		const raw = localStorage.getItem(TRANSCRIPT_CACHE_STORAGE_KEY) ?? "";
		expect(raw.length).toBeLessThanOrEqual(MAX_SESSION_CHARS + 2_000);
		const loaded = loadCachedTranscript("dsh", ["big"]);
		expect(JSON.stringify(loaded).length).toBeLessThanOrEqual(
			MAX_SESSION_CHARS,
		);
	});

	it("evicts the oldest conversations past the entry cap", () => {
		for (let index = 0; index < MAX_CACHE_ENTRIES + 5; index++) {
			saveCachedTranscript("dsh", `s-${index}`, [
				userLine(`u-${index}`, `question ${index}`),
				agentLine(`a-${index}`, [textPart(`answer ${index}`)]),
			]);
		}

		const raw = JSON.parse(
			localStorage.getItem(TRANSCRIPT_CACHE_STORAGE_KEY) ?? "{}",
		) as { entries: Record<string, unknown> };
		expect(Object.keys(raw.entries).length).toBeLessThanOrEqual(
			MAX_CACHE_ENTRIES,
		);
		expect(loadCachedTranscript("dsh", ["s-0"])).toBeNull();
		expect(
			loadCachedTranscript("dsh", [`s-${MAX_CACHE_ENTRIES + 4}`]),
		).not.toBeNull();
	});

	it("survives a quota failure by dropping old conversations", () => {
		vi.stubGlobal("localStorage", memoryStorage({ quota: 60_000 }));
		saveCachedTranscript("dsh", "old", [
			userLine("u-old", "question"),
			agentLine("a-old", [textPart("z".repeat(20_000))]),
		]);
		saveCachedTranscript("dsh", "older", [
			userLine("u-older", "question"),
			agentLine("a-older", [textPart("z".repeat(20_000))]),
		]);

		expect(() =>
			saveCachedTranscript("dsh", "new", [
				userLine("u-new", "question"),
				agentLine("a-new", [textPart("z".repeat(30_000))]),
			]),
		).not.toThrow();
		expect(loadCachedTranscript("dsh", ["new"])).not.toBeNull();
	});

	it("supports removing one conversation and clearing everything", () => {
		saveCachedTranscript("dsh", "keep", [userLine("u1", "kept")]);
		saveCachedTranscript("dsh", "drop", [userLine("u2", "dropped")]);

		removeCachedTranscript("dsh", ["drop"]);
		expect(loadCachedTranscript("dsh", ["drop"])).toBeNull();
		expect(loadCachedTranscript("dsh", ["keep"])).not.toBeNull();

		clearCachedTranscripts();
		expect(loadCachedTranscript("dsh", ["keep"])).toBeNull();
	});
});

// ---------------------------------------------------------------------------
// Merge priority: provider/durable transcript wins
// ---------------------------------------------------------------------------

describe("mergeTranscriptLines", () => {
	it("never overrides assistant text that the provider transcript has", () => {
		const durable = [
			userLine("u1", "question"),
			agentLine("a1", [textPart("answer from disk")]),
		];
		const cached = [
			userLine("cu1", "question"),
			agentLine("ca1", [textPart("answer from cache")]),
		];

		const merged = mergeTranscriptLines(durable, cached);

		expect(merged).toBe(durable);
		expect(transcriptText(merged)).not.toContain("cache");
	});

	it("fills assistant text the interrupted provider turn never persisted", () => {
		const durable = interruptedTurn();
		const cached = [
			userLine("cu1", "Explain the paper"),
			agentLine("ca1", [
				reasoningPart("thinking"),
				toolPart("tool-1", "failed"),
				textPart("the streamed final answer"),
			]),
		];

		const merged = mergeTranscriptLines(durable, cached);

		expect(hasVisibleAssistantText(merged)).toBe(true);
		expect(transcriptText(merged)).toContain("the streamed final answer");
		const agent = merged[1];
		expect(agent?.kind).toBe("agent");
		if (agent?.kind !== "agent") return;
		// Durable order stays: reasoning, tool, then the recovered text.
		expect(agent.parts[0]?.type).toBe("reasoning");
		expect(agent.parts[1]?.type).toBe("tool");
		expect(agent.parts[2]?.type).toBe("text");
		expect(agent.streaming).toBe(false);
		// The pending tool from the interrupted turn is settled from the cache.
		const tool = agent.parts[1];
		expect(tool?.type === "tool" ? tool.tool.status : "missing").toBe("failed");
	});

	it("appends a cached turn that the provider transcript lost entirely", () => {
		const durable = [userLine("u1", "first question")];
		const cached = [
			userLine("cu1", "first question"),
			agentLine("ca1", [textPart("first answer")]),
		];

		const merged = mergeTranscriptLines(durable, cached);

		expect(merged.filter((line) => line.kind === "user")).toHaveLength(1);
		expect(transcriptText(merged)).toContain("first answer");
	});

	it("keeps earlier durable answers and only appends the newest cached turn", () => {
		const durable: ChatLine[] = [
			userLine("u1", "first"),
			agentLine("a1", [textPart("disk first")]),
			userLine("u2", "second"),
			agentLine("a2", [reasoningPart("still thinking")]),
		];
		const cached: ChatLine[] = [
			userLine("cu1", "first"),
			agentLine("ca1", [textPart("cache first")]),
			userLine("cu2", "second"),
			agentLine("ca2", [textPart("cache second")]),
			userLine("cu3", "third"),
			agentLine("ca3", [textPart("cache third")]),
		];

		const merged = mergeTranscriptLines(durable, cached);
		const text = transcriptText(merged);

		expect(text).toContain("disk first");
		expect(text).not.toContain("cache first");
		expect(text).toContain("cache second");
		expect(text).toContain("cache third");
		expect(merged.filter((line) => line.kind === "user")).toHaveLength(3);
	});

	it("preserves durable notice / error lines while restoring text", () => {
		const durable: ChatLine[] = [
			userLine("u1", "question"),
			agentLine("a1", [reasoningPart("thinking")]),
			{ id: "n1", kind: "notice", text: "no saved answer" },
		];
		const cached = [
			userLine("cu1", "question"),
			agentLine("ca1", [reasoningPart("thinking"), textPart("recovered")]),
		];

		const merged = mergeTranscriptLines(durable, cached);

		expect(merged.some((line) => line.kind === "notice")).toBe(true);
		expect(transcriptText(merged)).toContain("recovered");
	});

	it("returns the durable transcript when the cache has no assistant text", () => {
		const durable = [userLine("u1", "question")];
		const cached = [userLine("cu1", "question")];
		expect(mergeTranscriptLines(durable, cached)).toBe(durable);
	});

	it("restores a cached transcript when the provider replayed nothing", () => {
		const cached = [
			userLine("cu1", "question"),
			agentLine("ca1", [textPart("cached answer")]),
		];
		const merged = mergeTranscriptLines([], cached);
		expect(transcriptText(merged)).toBe("question\ncached answer");
	});
});

// ---------------------------------------------------------------------------
// Resolve after refresh (runtime id → provider id)
// ---------------------------------------------------------------------------

describe("cached transcript lookup", () => {
	it("finds the cache after a refresh through matching user turns", () => {
		// Written while streaming: the ACP provider id was never learned because
		// the turn died before agent:completed.
		saveCachedTranscript("dsh", "runtime-rt-1", [
			userLine("u1", "Explain the paper"),
			agentLine("a1", [textPart("the streamed final answer")]),
		]);
		// After refresh the history row is keyed by the durable provider session.
		const durable = interruptedTurn();

		const cached = resolveCachedTranscript({
			agentId: "dsh",
			sessionIds: ["provider-ps-1", "provider-ps-1"],
			referenceLines: durable,
		});

		expect(cached).not.toBeNull();
		const merged = mergeTranscriptLines(durable, cached ?? []);
		expect(transcriptText(merged)).toContain("the streamed final answer");
	});

	it("prefers an exact provider-id entry over a turn match", () => {
		saveCachedTranscript("dsh", "provider-ps-1", [
			userLine("u1", "Explain the paper"),
			agentLine("a1", [textPart("exact entry")]),
		]);
		saveCachedTranscript("dsh", "runtime-rt-1", [
			userLine("u1", "Explain the paper"),
			agentLine("a1", [textPart("fallback entry")]),
		]);

		const cached = resolveCachedTranscript({
			agentId: "dsh",
			sessionIds: ["provider-ps-1", "runtime-rt-1"],
			referenceLines: interruptedTurn(),
		});

		expect(transcriptText(cached ?? [])).toContain("exact entry");
	});

	it("falls back when the exact entry only holds a textless snapshot", () => {
		// Exact provider key has only the user turn (written before the answer).
		saveCachedTranscript("dsh", "provider-ps", [
			userLine("u1", "Explain the paper"),
		]);
		// The interrupted runtime-key entry holds the streamed answer.
		saveCachedTranscript("dsh", "runtime-rt", [
			userLine("u1", "Explain the paper"),
			agentLine("a1", [textPart("the streamed final answer")]),
		]);

		const cached = resolveCachedTranscript({
			agentId: "dsh",
			sessionIds: ["provider-ps"],
			referenceLines: interruptedTurn(),
		});

		expect(transcriptText(cached ?? [])).toContain("the streamed final answer");
	});

	it("refuses to guess between conversations sharing the same user prompt", () => {
		saveCachedTranscript("dsh", "runtime-a", [
			userLine("u1", "继续"),
			agentLine("a1", [textPart("answer A")]),
		]);
		saveCachedTranscript("dsh", "runtime-b", [
			userLine("u1", "继续"),
			agentLine("a1", [textPart("answer B")]),
		]);

		const found = findCachedTranscriptByUserTurns("dsh", [
			userLine("du1", "继续"),
			agentLine("da1", [reasoningPart("thinking")]),
		]);
		expect(found).toBeNull();
	});

	it("treats duplicate runtime/provider entries as one candidate", () => {
		const lines = [
			userLine("u1", "Explain the paper"),
			agentLine("a1", [textPart("the streamed final answer")]),
		];
		saveCachedTranscript("dsh", "runtime-rt", lines);
		saveCachedTranscript("dsh", "provider-ps", lines);

		const found = findCachedTranscriptByUserTurns("dsh", interruptedTurn());
		expect(transcriptText(found ?? [])).toContain("the streamed final answer");
	});

	it("does not borrow a conversation with a different first prompt", () => {
		saveCachedTranscript("dsh", "runtime-rt-1", [
			userLine("u1", "Another question"),
			agentLine("a1", [textPart("another answer")]),
		]);

		const found = findCachedTranscriptByUserTurns("dsh", interruptedTurn());
		expect(found).toBeNull();
	});
});

// ---------------------------------------------------------------------------
// Debounced save scheduler
// ---------------------------------------------------------------------------

describe("scheduleTranscriptSave", () => {
	it("debounces streaming updates into one write", () => {
		vi.useFakeTimers();
		const first = [userLine("u1", "partial")];
		const second = [userLine("u1", "partial answer")];

		scheduleTranscriptSave("dsh", "s1", first);
		vi.advanceTimersByTime(TRANSCRIPT_SAVE_DEBOUNCE_MS / 2);
		scheduleTranscriptSave("dsh", "s1", second);
		vi.advanceTimersByTime(TRANSCRIPT_SAVE_DEBOUNCE_MS / 2);

		expect(loadCachedTranscript("dsh", ["s1"])).toBeNull();

		vi.advanceTimersByTime(TRANSCRIPT_SAVE_DEBOUNCE_MS);
		expect(transcriptText(loadCachedTranscript("dsh", ["s1"]) ?? [])).toBe(
			"partial answer",
		);
	});

	it("flushes continuously rescheduled saves at the hard deadline", () => {
		vi.useFakeTimers();
		const lines = [
			userLine("u1", "question"),
			agentLine("a1", [textPart("streaming")]),
		];

		scheduleTranscriptSave("dsh", "s1", lines);
		// Keep rescheduling every 400 ms (< debounce): the 2 s cap must fire.
		for (let index = 0; index < 4; index++) {
			vi.advanceTimersByTime(400);
			scheduleTranscriptSave("dsh", "s1", lines);
		}
		vi.advanceTimersByTime(399);
		// 1999 ms elapsed; last schedule reset the debounce timer.
		expect(loadCachedTranscript("dsh", ["s1"])).toBeNull();
		vi.advanceTimersByTime(1);
		expect(loadCachedTranscript("dsh", ["s1"])).not.toBeNull();
	});

	it("migrates the runtime-id entry to the provider-id entry", () => {
		vi.useFakeTimers();
		const lines = [userLine("u1", "question")];
		scheduleTranscriptSave("dsh", "runtime-rt", lines);
		vi.advanceTimersByTime(TRANSCRIPT_SAVE_DEBOUNCE_MS);
		expect(loadCachedTranscript("dsh", ["runtime-rt"])).not.toBeNull();

		scheduleTranscriptSave("dsh", "provider-ps", lines, {
			dropSessionIds: ["runtime-rt"],
		});
		flushScheduledTranscriptSaves();

		expect(loadCachedTranscript("dsh", ["provider-ps"])).not.toBeNull();
		expect(loadCachedTranscript("dsh", ["runtime-rt"])).toBeNull();
	});

	it("keeps the runtime-id copy when the provider-id write fails", () => {
		vi.useFakeTimers();
		vi.stubGlobal("localStorage", memoryStorage({ quota: 3_000 }));
		saveCachedTranscript("dsh", "runtime-rt", [userLine("u1", "question")]);

		scheduleTranscriptSave(
			"dsh",
			"provider-ps",
			[
				userLine("u1", "question"),
				agentLine("a1", [textPart("y".repeat(5_000))]),
			],
			{ dropSessionIds: ["runtime-rt"] },
		);
		flushScheduledTranscriptSaves();

		// The migrated write exceeded the quota, so the old copy must survive.
		expect(loadCachedTranscript("dsh", ["runtime-rt"])).not.toBeNull();
		expect(loadCachedTranscript("dsh", ["provider-ps"])).toBeNull();
	});

	it("flushes pending saves on demand and can discard them", () => {
		vi.useFakeTimers();
		const lines = [userLine("u1", "question")];

		scheduleTranscriptSave("dsh", "s1", lines);
		flushScheduledTranscriptSaves();
		expect(loadCachedTranscript("dsh", ["s1"])).not.toBeNull();

		scheduleTranscriptSave("dsh", "s2", lines);
		discardScheduledTranscriptSaves();
		vi.advanceTimersByTime(10_000);
		expect(loadCachedTranscript("dsh", ["s2"])).toBeNull();
	});

	it("is a no-op when storage is unavailable", () => {
		vi.stubGlobal("localStorage", undefined);
		expect(() =>
			saveCachedTranscript("dsh", "s1", [userLine("u1", "q")]),
		).not.toThrow();
		expect(loadCachedTranscript("dsh", ["s1"])).toBeNull();
	});
});

// ---------------------------------------------------------------------------
// Store mirror (used by the agent panel hook)
// ---------------------------------------------------------------------------

describe("createTranscriptPersister", () => {
	it("saves streamed lines under the runtime id, then under the provider id", () => {
		vi.useFakeTimers();
		const persister = createTranscriptPersister();
		const streaming: ChatLine[] = [
			userLine("u1", "question"),
			{
				id: "a1",
				kind: "agent",
				parts: [textPart("partial")],
				streaming: true,
			} as ChatLine,
		];
		persister.sync([
			{
				id: "runtime-rt",
				agentId: "dsh",
				providerSessionId: null,
				lines: streaming,
			},
		]);
		vi.advanceTimersByTime(TRANSCRIPT_SAVE_DEBOUNCE_MS);
		expect(
			transcriptText(loadCachedTranscript("dsh", ["runtime-rt"]) ?? []),
		).toBe("question\npartial");

		const completed: ChatLine[] = [
			userLine("u1", "question"),
			agentLine("a1", [textPart("final answer")]),
		];
		// agent:completed publishes the durable provider session id.
		persister.sync([
			{
				id: "runtime-rt",
				agentId: "dsh",
				providerSessionId: "provider-ps",
				lines: completed,
			},
		]);
		flushScheduledTranscriptSaves();

		expect(
			transcriptText(loadCachedTranscript("dsh", ["provider-ps"]) ?? []),
		).toBe("question\nfinal answer");
		// The stale runtime-id copy is migrated, not duplicated.
		expect(loadCachedTranscript("dsh", ["runtime-rt"])).toBeNull();
	});

	it("skips unchanged transcripts and empty line arrays", () => {
		vi.useFakeTimers();
		const persister = createTranscriptPersister();
		const lines: ChatLine[] = [userLine("u1", "question")];
		const sessions = [{ id: "s1", agentId: "dsh", lines }];

		persister.sync(sessions);
		flushScheduledTranscriptSaves();
		const stored = localStorage.getItem(TRANSCRIPT_CACHE_STORAGE_KEY) ?? "";
		const before = JSON.parse(stored) as {
			entries: Record<string, { at: number }>;
		};
		const firstStamp = before.entries[transcriptCacheKey("dsh", "s1")]?.at;

		persister.sync([...sessions]);
		flushScheduledTranscriptSaves();
		const after = JSON.parse(
			localStorage.getItem(TRANSCRIPT_CACHE_STORAGE_KEY) ?? "",
		) as { entries: Record<string, { at: number }> };
		expect(after.entries[transcriptCacheKey("dsh", "s1")]?.at).toBe(firstStamp);

		persister.sync([{ id: "empty", agentId: "dsh", lines: [] }]);
		flushScheduledTranscriptSaves();
		expect(loadCachedTranscript("dsh", ["empty"])).toBeNull();
	});

	it("does not forget cached transcripts when the panel scope resets", () => {
		vi.useFakeTimers();
		saveCachedTranscript("dsh", "runtime-rt", [
			userLine("u1", "question"),
			agentLine("a1", [textPart("answer")]),
		]);
		const persister = createTranscriptPersister();
		const lines = [userLine("u1", "question")];
		persister.sync([{ id: "runtime-rt", agentId: "dsh", lines }]);
		flushScheduledTranscriptSaves();

		// Agent switch / new chat / vault close clears the in-memory rows.
		persister.sync([]);
		flushScheduledTranscriptSaves();

		expect(loadCachedTranscript("dsh", ["runtime-rt"])).not.toBeNull();
	});
});
