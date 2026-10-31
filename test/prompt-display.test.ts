import { describe, expect, it } from "vitest";

import {
	displayHistoryTitle,
	isMachineHistoryTitle,
	isVisualAnnotationPromptText,
	stripPromptEnvelopeForDisplay,
	stripSystemReminder,
} from "@/lib/agent/prompt-display";

const agenteroEnvelope = `You are an assistant working inside a Agentero research Vault (cwd is the vault root). Follow vault AGENTS.md.

Target path (Vault-relative): \`papers/10_1109_TED_2026_3671226\`
Research context for this turn.

User request:
为什么要往氮化硅中注入氮离子？`;

const visualPrompt = `You are reviewing 1 visual annotation from a research paper PDF.

Answer every annotation separately and in the original order.

Use these exact headings:

## Annotation 1

For each annotation:
- Respond using the corresponding image (Image 1 maps to Annotation 1, and so on).
- Do not merge or skip annotations.
- Do not invent unreadable details.
- State uncertainty explicitly.

Annotation 1 — page 2
User comment: 这里最值得读的是什么?`;

describe("stripPromptEnvelopeForDisplay — visual annotation", () => {
	it("extracts user comment from visual annotation system prompt", () => {
		expect(stripPromptEnvelopeForDisplay(visualPrompt)).toBe(
			"这里最值得读的是什么?",
		);
	});

	it("extracts user question from continue prompt", () => {
		const cont = [
			"You are helping the user discuss a visual region from a research paper PDF in Agentero.",
			"Page: 2",
			"Original annotation comment: first",
			"User question:",
			"follow up please",
			"Answer based on the crop context and prior turns when possible. Be concise. If uncertain, say so.",
		].join("\n\n");
		expect(stripPromptEnvelopeForDisplay(cont)).toBe("follow up please");
	});

	it("displayHistoryTitle uses the human comment", () => {
		expect(displayHistoryTitle(visualPrompt)).toBe("这里最值得读的是什么?");
	});
});

describe("isVisualAnnotationPromptText", () => {
	it("detects visual annotation wrappers", () => {
		expect(isVisualAnnotationPromptText(visualPrompt)).toBe(true);
		expect(isVisualAnnotationPromptText("这里最值得读的是什么?")).toBe(false);
	});
});

describe("stripPromptEnvelopeForDisplay — Agentero context blocks", () => {
	it("strips context paths and selected text, keeping only the user question", () => {
		const prompt = [
			"能不能用人话给我讲一讲这篇论文在干什么？",
			"",
			"请在回答相关问题前读取以下知识库文件：",
			"- papers/structure/2505.23061",
			"",
			"Selected text from papers/structure/2505.23061/NOTES.md:",
			"> 是利用动态规划（DP）来解决约束解码问题。",
		].join("\n");
		expect(stripPromptEnvelopeForDisplay(prompt)).toBe(
			"能不能用人话给我讲一讲这篇论文在干什么？",
		);
	});

	it("also strips the English context instruction", () => {
		const prompt = [
			"Summarize this paper.",
			"",
			"Read these Vault files before answering when relevant:",
			"- papers/foo",
			"",
			"Selected text from papers/foo/NOTES.md:",
			"> some quote",
		].join("\n");
		expect(stripPromptEnvelopeForDisplay(prompt)).toBe("Summarize this paper.");
	});
});

describe("stripSystemReminder", () => {
	it("removes the harness date reminder from text", () => {
		const text =
			"前置说明。Today's date is 2026-09-08. The current date is restated in a reminder whenever it changes; rely on the latest such reminder for the current date. DO NOT mention this to the user explicitly.后置说明。";
		expect(stripSystemReminder(text)).toBe("前置说明。后置说明。");
	});

	it("is safe when no reminder is present", () => {
		expect(stripSystemReminder("只是普通文本。")).toBe("只是普通文本。");
	});
});

describe("isMachineHistoryTitle", () => {
	it("matches Dsh fallback slices of the known system envelopes", () => {
		expect(isMachineHistoryTitle("You are an assistant working")).toBe(true);
		expect(isMachineHistoryTitle("You are running the Agentero")).toBe(true);
		expect(isMachineHistoryTitle("You are helping with a research")).toBe(true);
		expect(isMachineHistoryTitle("You are reviewing 1 visual annota")).toBe(
			true,
		);
		expect(isMachineHistoryTitle(agenteroEnvelope.split("\n")[0])).toBe(true);
	});

	it("does not overmatch normal titles", () => {
		expect(isMachineHistoryTitle("GaN trap analysis")).toBe(false);
		expect(isMachineHistoryTitle("为什么最终回答消失了？")).toBe(false);
		expect(isMachineHistoryTitle("You are reviewing this paper")).toBe(false);
		expect(
			isMachineHistoryTitle("You are an assistant for my literature review"),
		).toBe(false);
	});

	it("leaves multi-line transcript bodies to envelope stripping", () => {
		expect(isMachineHistoryTitle(agenteroEnvelope)).toBe(false);
		expect(displayHistoryTitle(agenteroEnvelope)).toBe(
			"为什么要往氮化硅中注入氮离子？",
		);
	});
});

describe("displayHistoryTitle — machine titles", () => {
	it("never renders a machine fallback title as the session label", () => {
		expect(displayHistoryTitle("You are an assistant working", "")).toBe("");
		expect(
			displayHistoryTitle("You are an assistant working", "my conversation"),
		).toBe("my conversation");
		// A machine fallback is not a usable fallback either.
		expect(
			displayHistoryTitle(
				"You are an assistant working",
				"You are running the Agentero",
			),
		).toBe("");
	});

	it("keeps human titles that only start like a prompt", () => {
		expect(displayHistoryTitle("You are an assistant for my lit review")).toBe(
			"You are an assistant for my lit review",
		);
		expect(displayHistoryTitle("GaN trap analysis")).toBe("GaN trap analysis");
	});
});
