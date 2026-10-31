import { beforeEach, describe, expect, it, vi } from "vitest";
import {
	historyLacksSavedAnswer,
	hydrateSessionTitles,
	isSessionIdPrefixTitle,
	mergeImportedSessions,
	renameHistorySessionTitle,
	sanitizeChatLines,
	sessionsNeedingTitleHydration,
	titleFromLoadedHistory,
} from "@/components/agent/hooks/use-agent-history";
import * as agentApi from "@/lib/agent";
import {
	type AgentSessionRecord,
	agentSessionStore,
} from "@/lib/agent/agent-session-store";
import type { AcpSessionInfo } from "@/lib/agent/api";
import * as titleCache from "@/lib/agent/history-title-cache";

vi.mock("@/lib/agent", async (importOriginal) => {
	const actual = await importOriginal<typeof import("@/lib/agent")>();
	return {
		...actual,
		loadSession: vi.fn(),
	};
});

function makeRecord(
	opts: Partial<AgentSessionRecord> & { id: string },
): AgentSessionRecord {
	return {
		id: opts.id,
		agentId: opts.agentId ?? "agent-1",
		source: opts.source ?? "external",
		title: opts.title ?? "",
		agentName: opts.agentName ?? "Agent",
		startedAt: opts.startedAt ?? "2026/8/21 19:11:00",
		lines: opts.lines ?? [],
		status: opts.status ?? "completed",
		providerSessionId: opts.providerSessionId ?? null,
		...opts,
	};
}

function makeAcpSession(
	opts: Partial<AcpSessionInfo> & { sessionId: string },
): AcpSessionInfo {
	return {
		sessionId: opts.sessionId,
		cwd: opts.cwd ?? "/vault",
		title: opts.title ?? null,
		updatedAt: opts.updatedAt ?? "2026-08-21T19:11:00Z",
		...opts,
	};
}

describe("sanitizeChatLines", () => {
	it("preserves visualAnnotations and images on user turns", () => {
		const lines = [
			{
				id: "l1",
				kind: "user" as const,
				text: "explain",
				visualAnnotations: [
					{
						id: "v1",
						page: 1,
						comment: "region",
						image: { data: "base64", mimeType: "image/png" },
					},
				],
				images: [{ data: "base64", mimeType: "image/png" }],
			},
		];
		const out = sanitizeChatLines(lines);
		expect(out).toHaveLength(1);
		expect(out[0]?.visualAnnotations).toHaveLength(1);
		expect(out[0]?.visualAnnotations?.[0]?.id).toBe("v1");
		expect(out[0]?.images).toHaveLength(1);
	});

	it("drops empty user turns that have no text, visuals, or images", () => {
		const lines = [
			{
				id: "l1",
				kind: "user" as const,
				text: "   ",
			},
		];
		expect(sanitizeChatLines(lines)).toHaveLength(0);
	});

	it("keeps user turns that are visual-only", () => {
		const lines = [
			{
				id: "l1",
				kind: "user" as const,
				text: "",
				visualAnnotations: [
					{
						id: "v1",
						page: 1,
						comment: "region",
						image: { data: "base64", mimeType: "image/png" },
					},
				],
			},
		];
		expect(sanitizeChatLines(lines)).toHaveLength(1);
	});
});

describe("isSessionIdPrefixTitle", () => {
	it("matches the historical 8-char slice and leading Kimi-style leftovers", () => {
		expect(isSessionIdPrefixTitle("ses_3a2a", "ses_3a2abcdef")).toBe(true);
		expect(isSessionIdPrefixTitle("ses_3a2", "ses_3a2abcdef")).toBe(true);
		expect(isSessionIdPrefixTitle("ses_3a2abcdef", "ses_3a2abcdef")).toBe(true);
		expect(
			isSessionIdPrefixTitle("Summarize this paper", "ses_3a2abcdef"),
		).toBe(false);
	});
});

describe("titleFromLoadedHistory", () => {
	it("uses the first user turn when present", () => {
		const history = {
			sessionId: "s1",
			lines: [
				{ id: "l1", kind: "agent" as const, text: "hello" },
				{ id: "l2", kind: "user" as const, text: "summarize this paper" },
			],
		};
		expect(titleFromLoadedHistory(history)).toBe("summarize this paper");
	});

	it("falls back to the ACP-provided title when there is no user turn", () => {
		const history = {
			sessionId: "s1",
			title: "ACP title",
			lines: [{ id: "l1", kind: "agent" as const, text: "hello" }],
		};
		expect(titleFromLoadedHistory(history)).toBe("ACP title");
	});

	it("returns empty string when neither user turn nor ACP title exists", () => {
		const history = {
			sessionId: "s1",
			lines: [{ id: "l1", kind: "agent" as const, text: "hello" }],
		};
		expect(titleFromLoadedHistory(history)).toBe("");
	});
});

describe("mergeImportedSessions", () => {
	it("flags untitled external sessions as hydration candidates", () => {
		const prev: AgentSessionRecord[] = [];
		const chatSessions = [makeAcpSession({ sessionId: "s1", title: null })];

		const { sessions, hydrationCandidates } = mergeImportedSessions(
			prev,
			chatSessions,
			"agent-1",
			"Agent",
			"zh-CN",
		);

		expect(sessions).toHaveLength(1);
		// Empty until hydration / open — never seed with the session-id prefix (#484).
		expect(sessions[0].title).toBe("");
		expect(hydrationCandidates).toHaveLength(1);
		expect(hydrationCandidates[0].id).toBe("s1");
	});

	it("does not flag titled external sessions", () => {
		const prev: AgentSessionRecord[] = [];
		const chatSessions = [
			makeAcpSession({ sessionId: "s1", title: "Already named" }),
		];

		const { sessions, hydrationCandidates } = mergeImportedSessions(
			prev,
			chatSessions,
			"agent-1",
			"Agent",
			"zh-CN",
		);

		expect(sessions[0].title).toBe("Already named");
		expect(hydrationCandidates).toHaveLength(0);
	});

	it("derives title from first local user turn when ACP title is empty", () => {
		const prev = [
			makeRecord({
				id: "s1",
				source: "local",
				title: "Local title",
				lines: [
					{
						id: "l1",
						kind: "user" as const,
						text: "local question",
					},
				],
				providerSessionId: "s1",
			}),
		];
		const chatSessions = [makeAcpSession({ sessionId: "s1", title: null })];

		const { sessions, hydrationCandidates } = mergeImportedSessions(
			prev,
			chatSessions,
			"agent-1",
			"Agent",
			"zh-CN",
		);

		expect(sessions[0].title).toBe("local question");
		expect(hydrationCandidates).toHaveLength(0);
	});

	it("drops session-id placeholder titles and flags for hydration", () => {
		const prev = [
			makeRecord({
				id: "s1abcdef",
				source: "local",
				title: "s1abcdef".slice(0, 8),
				lines: [],
				providerSessionId: "s1abcdef",
			}),
		];
		const chatSessions = [
			makeAcpSession({ sessionId: "s1abcdef", title: null }),
		];

		const { sessions, hydrationCandidates } = mergeImportedSessions(
			prev,
			chatSessions,
			"agent-1",
			"Agent",
			"zh-CN",
		);

		expect(sessions[0].title).toBe("");
		expect(hydrationCandidates).toHaveLength(1);
		expect(hydrationCandidates[0].id).toBe("s1abcdef");
	});

	it("keeps a prior human title when there are no lines and ACP title is empty", () => {
		const prev = [
			makeRecord({
				id: "s1",
				source: "external",
				title: "Earlier hydrated title",
				lines: [],
				providerSessionId: "s1",
			}),
		];
		const chatSessions = [makeAcpSession({ sessionId: "s1", title: null })];

		const { sessions, hydrationCandidates } = mergeImportedSessions(
			prev,
			chatSessions,
			"agent-1",
			"Agent",
			"zh-CN",
		);

		expect(sessions[0].title).toBe("Earlier hydrated title");
		expect(hydrationCandidates).toHaveLength(0);
	});

	it("keeps a user-renamed title over a newer ACP title (#710)", () => {
		vi.spyOn(titleCache, "getOverriddenHistoryTitle").mockReturnValue(
			"My rename",
		);
		const prev = [
			makeRecord({
				id: "s1",
				source: "external",
				title: "My rename",
				titleLocked: true,
				lines: [],
				providerSessionId: "s1",
			}),
		];
		const chatSessions = [
			makeAcpSession({ sessionId: "s1", title: "ACP pushed title" }),
		];

		const { sessions, hydrationCandidates } = mergeImportedSessions(
			prev,
			chatSessions,
			"agent-1",
			"Agent",
			"zh-CN",
		);

		expect(sessions[0].title).toBe("My rename");
		expect(sessions[0].titleLocked).toBe(true);
		expect(hydrationCandidates).toHaveLength(0);
		vi.mocked(titleCache.getOverriddenHistoryTitle).mockRestore();
	});
});

describe("sessionsNeedingTitleHydration", () => {
	it("includes empty and id-prefix titles, skips derived local lines", () => {
		const sessions = [
			makeRecord({ id: "a", title: "" }),
			makeRecord({
				id: "ses_abcdxxxx",
				title: "ses_abcd",
				providerSessionId: "ses_abcdxxxx",
			}),
			makeRecord({
				id: "c",
				title: "",
				lines: [{ id: "l1", kind: "user" as const, text: "hello" }],
			}),
			makeRecord({ id: "d", title: "Real title" }),
			makeRecord({ id: "e", title: "", titleLocked: true }),
		];
		const need = sessionsNeedingTitleHydration(sessions, "agent-1");
		expect(need.map((s) => s.id)).toEqual(["a", "ses_abcdxxxx"]);
	});
});

describe("renameHistorySessionTitle", () => {
	it("locks the title in the session store and writes an override (#710)", () => {
		const setUser = vi
			.spyOn(titleCache, "setUserHistoryTitle")
			.mockImplementation(() => {});
		const item = makeRecord({
			id: "s1",
			title: "Old",
			providerSessionId: "prov-1",
		});
		agentSessionStore.setState({ sessions: [item], activeTabId: "s1" });

		expect(renameHistorySessionTitle(item, "  New title  ")).toBe("New title");
		expect(setUser).toHaveBeenCalledWith(
			"agent-1",
			"s1",
			"New title",
			"prov-1",
		);
		const updated = agentSessionStore
			.getState()
			.sessions.find((session) => session.id === "s1");
		expect(updated?.title).toBe("New title");
		expect(updated?.titleLocked).toBe(true);
		setUser.mockRestore();
	});
});

describe("hydrateSessionTitles", () => {
	beforeEach(() => {
		vi.resetAllMocks();
		vi.spyOn(titleCache, "getCachedHistoryTitle").mockReturnValue(null);
		vi.spyOn(titleCache, "setCachedHistoryTitle").mockImplementation(() => {});
	});

	it("loads untitled sessions and updates their titles", async () => {
		const mockedLoadSession = vi.mocked(agentApi.loadSession);
		mockedLoadSession.mockResolvedValue({
			sessionId: "s1",
			lines: [
				{ id: "l1", kind: "user" as const, text: "what is the main claim?" },
			],
		});

		const setSessionHistory = vi.fn();
		const historyGenRef = { current: 1 };
		const items = [makeRecord({ id: "s1", providerSessionId: "s1" })];

		await hydrateSessionTitles(items, {
			generation: 1,
			historyGenRef,
			selectedAgentId: "agent-1",
			vaultPath: "/vault",
			setSessionHistory,
		});

		expect(mockedLoadSession).toHaveBeenCalledWith({
			agentId: "agent-1",
			sessionId: "s1",
			vaultPath: "/vault",
		});

		expect(setSessionHistory).toHaveBeenCalledTimes(1);
		const updater = setSessionHistory.mock.calls[0][0] as (
			prev: AgentSessionRecord[],
		) => AgentSessionRecord[];
		const next = updater(items);
		expect(next[0].title).toBe("what is the main claim?");
		expect(titleCache.setCachedHistoryTitle).toHaveBeenCalledWith(
			"agent-1",
			"s1",
			"what is the main claim?",
		);
	});

	it("uses the title cache and skips session/load", async () => {
		vi.mocked(titleCache.getCachedHistoryTitle).mockReturnValue("cached title");
		const mockedLoadSession = vi.mocked(agentApi.loadSession);
		const setSessionHistory = vi.fn();
		const historyGenRef = { current: 1 };
		const items = [makeRecord({ id: "s1", providerSessionId: "s1" })];

		await hydrateSessionTitles(items, {
			generation: 1,
			historyGenRef,
			selectedAgentId: "agent-1",
			vaultPath: "/vault",
			setSessionHistory,
		});

		expect(mockedLoadSession).not.toHaveBeenCalled();
		const updater = setSessionHistory.mock.calls[0][0] as (
			prev: AgentSessionRecord[],
		) => AgentSessionRecord[];
		expect(updater(items)[0].title).toBe("cached title");
	});

	it("does nothing when generation changes", async () => {
		const mockedLoadSession = vi.mocked(agentApi.loadSession);
		const setSessionHistory = vi.fn();
		const historyGenRef = { current: 2 };

		await hydrateSessionTitles([makeRecord({ id: "s1" })], {
			generation: 1,
			historyGenRef,
			selectedAgentId: "agent-1",
			vaultPath: null,
			setSessionHistory,
		});

		expect(mockedLoadSession).not.toHaveBeenCalled();
		expect(setSessionHistory).not.toHaveBeenCalled();
	});

	it("ignores failed loads silently", async () => {
		const mockedLoadSession = vi.mocked(agentApi.loadSession);
		mockedLoadSession.mockRejectedValue(new Error("network"));

		const setSessionHistory = vi.fn();
		const historyGenRef = { current: 1 };

		await hydrateSessionTitles([makeRecord({ id: "s1" })], {
			generation: 1,
			historyGenRef,
			selectedAgentId: "agent-1",
			vaultPath: null,
			setSessionHistory,
		});

		expect(mockedLoadSession).toHaveBeenCalledTimes(1);
		expect(setSessionHistory).not.toHaveBeenCalled();
	});
});

const MACHINE_TITLE = "You are an assistant working";

describe("mergeImportedSessions — machine titles", () => {
	it("ignores a machine ACP title and flags the session for hydration", () => {
		const { sessions, hydrationCandidates } = mergeImportedSessions(
			[],
			[makeAcpSession({ sessionId: "s1", title: MACHINE_TITLE })],
			"agent-1",
			"Agent",
			"zh-CN",
		);
		expect(sessions[0].title).toBe("");
		expect(hydrationCandidates.map((s) => s.id)).toEqual(["s1"]);
	});

	it("replaces a stored machine title instead of keeping it", () => {
		const prev = [
			makeRecord({
				id: "s1",
				source: "external",
				title: MACHINE_TITLE,
				providerSessionId: "s1",
			}),
		];
		const { sessions } = mergeImportedSessions(
			prev,
			[makeAcpSession({ sessionId: "s1", title: "Human ACP title" })],
			"agent-1",
			"Agent",
			"zh-CN",
		);
		expect(sessions[0].title).toBe("Human ACP title");
	});
});

describe("sessionsNeedingTitleHydration — machine titles", () => {
	it("includes machine fallback titles and skips real ones", () => {
		const sessions = [
			makeRecord({ id: "a", title: MACHINE_TITLE }),
			makeRecord({ id: "b", title: "GaN trap analysis" }),
			makeRecord({ id: "c", title: "You are reviewing this paper" }),
		];
		expect(
			sessionsNeedingTitleHydration(sessions, "agent-1").map((s) => s.id),
		).toEqual(["a"]);
	});
});

describe("hydrateSessionTitles — machine titles", () => {
	beforeEach(() => {
		vi.resetAllMocks();
		vi.spyOn(titleCache, "getCachedHistoryTitle").mockReturnValue(null);
		vi.spyOn(titleCache, "setCachedHistoryTitle").mockImplementation(() => {});
	});

	it("rehydrates a stale machine title from the first user turn", async () => {
		vi.mocked(titleCache.getCachedHistoryTitle).mockReturnValue(MACHINE_TITLE);
		vi.mocked(agentApi.loadSession).mockResolvedValue({
			sessionId: "s1",
			title: MACHINE_TITLE,
			lines: [{ id: "l1", kind: "user" as const, text: "real question" }],
		});
		const setSessionHistory = vi.fn();
		const items = [makeRecord({ id: "s1", title: MACHINE_TITLE })];

		await hydrateSessionTitles(items, {
			generation: 1,
			historyGenRef: { current: 1 },
			selectedAgentId: "agent-1",
			vaultPath: "/vault",
			setSessionHistory,
		});

		expect(agentApi.loadSession).toHaveBeenCalledTimes(1);
		expect(titleCache.setCachedHistoryTitle).toHaveBeenCalledWith(
			"agent-1",
			"s1",
			"real question",
		);
	});

	it("does not cache a machine title when a transcript gives no user text", async () => {
		vi.mocked(agentApi.loadSession).mockResolvedValue({
			sessionId: "s1",
			title: MACHINE_TITLE,
			lines: [
				{ id: "l1", kind: "agent" as const, text: "", reasoning: "think" },
			],
		});
		const setSessionHistory = vi.fn();

		await hydrateSessionTitles([makeRecord({ id: "s1" })], {
			generation: 1,
			historyGenRef: { current: 1 },
			selectedAgentId: "agent-1",
			vaultPath: "/vault",
			setSessionHistory,
		});

		expect(titleCache.setCachedHistoryTitle).not.toHaveBeenCalled();
	});
});

describe("historyLacksSavedAnswer", () => {
	const user = { id: "u", kind: "user" as const, text: "question" };

	it("flags interrupted Dsh turns that saved reasoning but no answer", () => {
		expect(
			historyLacksSavedAnswer([
				user,
				{
					id: "a",
					kind: "agent" as const,
					parts: [
						{ type: "reasoning" as const, id: "r", text: "thinking" },
						{
							type: "tool" as const,
							id: "t",
							tool: {
								id: "t1",
								title: "read",
								kind: "read",
								status: "completed" as const,
							},
						},
					],
				},
			]),
		).toBe(true);
		// A session saved only its user turn is also an interrupted attempt.
		expect(historyLacksSavedAnswer([user])).toBe(true);
	});

	it("keeps normal transcripts and non-conversations unflagged", () => {
		expect(
			historyLacksSavedAnswer([
				user,
				{
					id: "a",
					kind: "agent" as const,
					parts: [{ type: "text" as const, id: "t", text: "final answer" }],
				},
			]),
		).toBe(false);
		expect(
			historyLacksSavedAnswer([
				{ id: "s", kind: "system" as const, text: "no messages" },
			]),
		).toBe(false);
		// Whitespace-only text does not count as a saved answer.
		expect(
			historyLacksSavedAnswer([
				user,
				{
					id: "a",
					kind: "agent" as const,
					parts: [{ type: "text" as const, id: "t", text: "   " }],
				},
			]),
		).toBe(true);
	});
});
