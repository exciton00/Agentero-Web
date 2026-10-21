#!/usr/bin/env node
// Minimal ACP agent over stdio, used only by agentero-host integration tests.
//
// ACP framing is line-delimited JSON-RPC *without* a "jsonrpc" field:
//   request:      {"id":0,"method":"initialize","params":{...}}
//   notification: {"method":"session/update","params":{...}}
//   response:     {"id":0,"result":{...}} / {"id":0,"error":{...}}
//
// FAKE_ACP_MODE=cancel delays the prompt response until `session/cancel`.
const fs = require("fs");
const readline = require("readline");

const mode = process.env.FAKE_ACP_MODE || "normal";
const rl = readline.createInterface({ input: process.stdin, terminal: false });
let pendingPrompt = null;
let cancelled = false;

const dump = process.env.FAKE_ACP_DUMP;

function send(message) {
  // ACP requires the JSON-RPC 2.0 envelope on every outgoing message.
  const line = JSON.stringify({ jsonrpc: "2.0", ...message });
  if (dump) fs.appendFileSync(dump, `OUT ${line}\n`);
  process.stdout.write(line + "\n");
}

function respond(id, result) {
  send({ id, result });
}

function notifyUpdate(sessionId, update) {
  send({
    method: "session/update",
    params: { sessionId, update },
  });
}

function chunk(sessionId, text, thought) {
  notifyUpdate(sessionId, {
    sessionUpdate: thought ? "agent_thought_chunk" : "agent_message_chunk",
    content: { type: "text", text },
  });
}

rl.on("line", (line) => {
  if (dump) fs.appendFileSync(dump, `IN ${line}\n`);
  let message;
  try {
    message = JSON.parse(line);
  } catch {
    return;
  }

  switch (message.method) {
    case "initialize":
      respond(message.id, {
        protocolVersion: 1,
        agentCapabilities: {
          loadSession: true,
          promptCapabilities: { image: true, audio: false, embeddedContext: true },
          sessionCapabilities: { list: {}, resume: {}, delete: {} },
        },
        agentInfo: { name: "fake-acp", version: "0.1.0" },
      });
      break;

    case "session/new":
      respond(message.id, {
        sessionId: "fake-session-1",
        configOptions: [],
      });
      break;

    case "session/load":
    case "session/resume":
      respond(message.id, { configOptions: [] });
      break;

    case "session/list":
      respond(message.id, {
        sessions: [
          {
            sessionId: "fake-session-1",
            cwd: (message.params && message.params.cwd) || process.cwd(),
            title: "Fake session",
            updatedAt: "2026-01-01T00:00:00Z",
          },
        ],
      });
      break;

    case "session/prompt": {
      const sessionId = message.params.sessionId;
      chunk(sessionId, "Hel");
      chunk(sessionId, "lo");
      chunk(sessionId, "thinking...", true);
      notifyUpdate(sessionId, {
        sessionUpdate: "tool_call",
        toolCallId: "tool-1",
        title: "Read NOTES.md",
        kind: "read",
        status: "completed",
        rawInput: { path: "NOTES.md" },
        rawOutput: { found: true },
      });
      notifyUpdate(sessionId, {
        sessionUpdate: "usage_update",
        used: 12,
        size: 1000,
      });

      if (mode === "cancel") {
        // Hold the turn open until the client sends session/cancel.
        pendingPrompt = message.id;
        setTimeout(() => {
          if (pendingPrompt === message.id) {
            pendingPrompt = null;
            respond(message.id, { stopReason: "end_turn" });
          }
        }, 30000);
      } else {
        setTimeout(() => respond(message.id, { stopReason: "end_turn" }), 20);
      }
      break;
    }

    case "session/cancel":
      cancelled = true;
      if (pendingPrompt !== null) {
        const id = pendingPrompt;
        pendingPrompt = null;
        respond(id, { stopReason: "cancelled" });
      }
      break;

    default:
      if (message.method && message.id !== undefined) {
        send({
          id: message.id,
          error: { code: -32601, message: `method not found: ${message.method}` },
        });
      }
      break;
  }
});

process.on("SIGTERM", () => process.exit(0));
process.on("exit", () => {
  if (cancelled) process.stderr.write("fake-acp: cancelled\n");
});
