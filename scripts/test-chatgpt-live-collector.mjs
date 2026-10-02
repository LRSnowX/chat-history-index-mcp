import assert from "node:assert/strict";
import { spawnSync } from "node:child_process";
import fs from "node:fs";
import net from "node:net";
import os from "node:os";
import path from "node:path";
import test from "node:test";

import {
  NativeAppToolsClient,
  appendMemoryCompilerHistory,
  bridgeMessages,
  bridgeThread,
  memoryCompilerHistorySummary,
  memoryAutoConfig,
  maybeScheduleMemoryCompiler,
  readCompleteThread,
  readMemoryCompilerHistory,
  safeScheduleMemoryCompiler,
} from "./chatgpt-live-collector.mjs";

function encodeNativeFrame(message) {
  const body = Buffer.from(JSON.stringify(message), "utf8");
  const frame = Buffer.allocUnsafe(body.length + 4);
  frame.writeUInt32LE(body.length, 0);
  body.copy(frame, 4);
  return frame;
}

test("bridgeThread maps ChatGPT sidebar metadata into the Rust discovery contract", () => {
  assert.deepEqual(
    bridgeThread({
      id: "thread-1",
      kind: "chatgpt",
      title: "Project chat",
      createdAt: 10,
      updatedAt: 20,
    }),
    {
      thread_id: "thread-1",
      kind: "chatgpt",
      title: "Project chat",
      create_time: 10,
      update_time: 20,
    },
  );
});

test("bridgeMessages reverses items within newest-first turns for one final Rust reversal", () => {
  const messages = bridgeMessages({
    id: "turn-1",
    status: "completed",
    startedAt: 10,
    completedAt: 20,
    items: [
      {
        type: "userMessage",
        id: "user-1",
        content: [{ type: "text", text: "question" }],
      },
      { type: "agentMessage", id: "assistant-1", text: "answer" },
    ],
  });
  assert.deepEqual(
    messages.map((message) => [message.message_id, message.role, message.text]),
    [
      ["assistant-1", "assistant", "answer"],
      ["user-1", "user", "question"],
    ],
  );
  assert.equal(messages[0].truncated, false);
  assert.equal(messages[1].truncated, false);
});

test("bridgeMessages fails closed near the App Tools per-message output cap", () => {
  const messages = bridgeMessages({
    id: "turn-large",
    status: "completed",
    startedAt: 10,
    completedAt: 20,
    items: [{ type: "agentMessage", id: "assistant-large", text: "x".repeat(19_990) }],
  });
  assert.equal(messages.length, 1);
  assert.equal(messages[0].truncated, true);
});

test("memory auto compiler is disabled by default and validates explicit bounds", () => {
  assert.deepEqual(memoryAutoConfig({}), {
    enabled: false,
    projects: [],
    scanLimit: 500,
    maxConversations: 1,
    maxMessages: 8,
    model: null,
  });
  assert.deepEqual(
    memoryAutoConfig({
      CHAT_HISTORY_MEMORY_AUTO_PROJECTS: " LEMonX,Arcos,LEMonX ",
      CHAT_HISTORY_MEMORY_AUTO_SCAN_LIMIT: "250",
      CHAT_HISTORY_MEMORY_AUTO_MAX_CONVERSATIONS: "2",
      CHAT_HISTORY_MEMORY_AUTO_MAX_MESSAGES: "6",
      CHAT_HISTORY_MEMORY_MODEL: " gpt-5.6-sol ",
    }),
    {
      enabled: true,
      projects: ["LEMonX", "Arcos"],
      scanLimit: 250,
      maxConversations: 2,
      maxMessages: 6,
      model: "gpt-5.6-sol",
    },
  );
  assert.throws(
    () => memoryAutoConfig({
      CHAT_HISTORY_MEMORY_AUTO_PROJECTS: "LEMonX",
      CHAT_HISTORY_MEMORY_AUTO_MAX_CONVERSATIONS: "0",
    }),
    /must be an integer between 1 and 10/,
  );
});

test("memory auto compiler schedules only after a successful import and explicit opt-in", () => {
  const calls = [];
  const fakeSpawn = (command, args, options) => {
    calls.push({ command, args, options });
    return { pid: 12345, unref() {} };
  };
  const enabledEnv = {
    CHAT_HISTORY_MEMORY_AUTO_PROJECTS: "LEMonX",
    CHAT_HISTORY_MEMORY_MODEL: "gpt-5.6-sol",
  };
  assert.deepEqual(
    maybeScheduleMemoryCompiler({ imported: 1 }, fakeSpawn, {}),
    { enabled: false, scheduled: false },
  );
  assert.deepEqual(
    maybeScheduleMemoryCompiler({ imported: 0 }, fakeSpawn, enabledEnv),
    { enabled: true, scheduled: false },
  );
  const scheduled = maybeScheduleMemoryCompiler({ imported: 2 }, fakeSpawn, enabledEnv);
  assert.equal(scheduled.enabled, true);
  assert.equal(scheduled.scheduled, true);
  assert.equal(scheduled.pid, 12345);
  assert.deepEqual(scheduled.projects, ["LEMonX"]);
  assert.equal(scheduled.model, "gpt-5.6-sol");
  assert.equal(calls.length, 1);
  assert.deepEqual(calls[0].args.slice(-1), ["--memory-compiler-worker"]);
  assert.equal(calls[0].options.detached, true);
  assert.equal(calls[0].options.stdio, "ignore");
});

test("memory auto compiler scheduling errors do not escape into ingestion", () => {
  const logged = [];
  const result = safeScheduleMemoryCompiler(
    { imported: 1 },
    () => {
      throw new Error("spawn failed");
    },
    { CHAT_HISTORY_MEMORY_AUTO_PROJECTS: "LEMonX" },
    (payload) => logged.push(payload),
  );
  assert.equal(result.enabled, true);
  assert.equal(result.scheduled, false);
  assert.match(result.error, /spawn failed/);
  assert.equal(logged.length, 1);
  assert.equal(logged[0].event, "memory_auto_compile_schedule_error");
});

test("memory compiler history is bounded and retains success/failure telemetry", () => {
  const root = fs.mkdtempSync(path.join(os.tmpdir(), "chat-history-memory-history-"));
  try {
    const historyPath = path.join(root, "memory-auto-compiler-history.json");
    for (let index = 0; index < 23; index += 1) {
      appendMemoryCompilerHistory({
        state: "completed",
        started_at: `2026-10-02T00:${String(index).padStart(2, "0")}:00Z`,
        completed_at: `2026-10-02T00:${String(index).padStart(2, "0")}:30Z`,
        model: "gpt-5.6-sol",
        results: [{
          project: "LEMonX",
          status: "ok",
          result: {
            model_attempts: 1,
            staged: [{ staged: { candidate_ids: ["one", "two"] } }],
            failures: [],
          },
        }],
      }, historyPath);
    }
    appendMemoryCompilerHistory({
      state: "degraded",
      checked_at: "2026-10-02T01:00:00Z",
      error: "compiler failed",
      results: [{ project: "LEMonX", status: "error", error: "compiler failed" }],
    }, historyPath);
    appendMemoryCompilerHistory({
      state: "degraded",
      checked_at: "2026-10-02T01:01:00Z",
      error: "compiler failed again",
      results: [{ project: "LEMonX", status: "error", error: "compiler failed again" }],
    }, historyPath);

    const history = readMemoryCompilerHistory(historyPath);
    assert.equal(history.total_runs, 25);
    assert.equal(history.runs.length, 20);
    assert.equal(history.last_success_at, "2026-10-02T00:22:30Z");
    assert.equal(history.last_failure_at, "2026-10-02T01:01:00Z");
    assert.equal(history.consecutive_failures, 2);
    assert.equal(history.runs.at(-1).state, "degraded");
    assert.equal(history.runs.at(-3).projects[0].staged_candidates, 2);

    const summary = memoryCompilerHistorySummary(history);
    assert.equal(summary.total_runs, 25);
    assert.equal(summary.recent_runs.length, 5);
    assert.equal(summary.consecutive_failures, 2);
  } finally {
    fs.rmSync(root, { recursive: true, force: true });
  }
});

test("memory compiler worker is isolated, bounded, and releases its pid lock", () => {
  const root = fs.mkdtempSync(path.join(os.tmpdir(), "chat-history-memory-worker-"));
  try {
    const bin = path.join(root, "bin");
    const cache = path.join(root, "cache");
    fs.mkdirSync(bin, { recursive: true });
    fs.mkdirSync(cache, { recursive: true });
    const callsPath = path.join(root, "calls.txt");
    const fakeCli = path.join(bin, "chat-history-cli");
    fs.writeFileSync(
      fakeCli,
      [
        "#!/bin/sh",
        `printf '%s\\n' "$*" >> "${callsPath}"`,
        `printf '%s\\n' '${JSON.stringify({
          project: "LEMonX",
          model_attempts: 0,
          staged: [],
          failures: [],
        })}'`,
        "",
      ].join("\n"),
      { mode: 0o755 },
    );
    const collector = path.join(path.dirname(new URL(import.meta.url).pathname), "chatgpt-live-collector.mjs");
    const run = spawnSync(process.execPath, [collector, "--memory-compiler-worker"], {
      encoding: "utf8",
      env: {
        ...process.env,
        CHAT_HISTORY_DATA_HOME: root,
        CHAT_HISTORY_MEMORY_AUTO_PROJECTS: "LEMonX",
        CHAT_HISTORY_MEMORY_AUTO_SCAN_LIMIT: "250",
        CHAT_HISTORY_MEMORY_AUTO_MAX_CONVERSATIONS: "1",
        CHAT_HISTORY_MEMORY_AUTO_MAX_MESSAGES: "6",
        CHAT_HISTORY_MEMORY_MODEL: "gpt-5.6-sol",
      },
    });
    assert.equal(run.status, 0, run.stderr);
    const calls = fs.readFileSync(callsPath, "utf8").trim().split("\n");
    assert.deepEqual(calls, [
      "memory-compile-project --project LEMonX --scan-limit 250 --max-conversations 1 --max-messages 6",
    ]);
    const status = JSON.parse(
      fs.readFileSync(path.join(cache, "memory-auto-compiler-status.json"), "utf8"),
    );
    assert.equal(status.state, "completed");
    assert.deepEqual(status.projects, ["LEMonX"]);
    assert.equal(status.model, "gpt-5.6-sol");
    assert.equal(status.results.length, 1);
    assert.equal(status.results[0].status, "ok");
    const history = JSON.parse(
      fs.readFileSync(path.join(cache, "memory-auto-compiler-history.json"), "utf8"),
    );
    assert.equal(history.total_runs, 1);
    assert.equal(history.consecutive_failures, 0);
    assert.equal(history.runs.length, 1);
    assert.equal(history.runs[0].state, "completed");
    assert.equal(history.runs[0].projects[0].project, "LEMonX");
    assert.equal(fs.existsSync(path.join(cache, "memory-auto-compiler.lock")), false);
  } finally {
    fs.rmSync(root, { recursive: true, force: true });
  }
});

test("readCompleteThread requests assistant outputs but indexes only conversation messages", async () => {
  const calls = [];
  const client = {
    async callTool(name, args, contextThreadId) {
      calls.push({ name, args, contextThreadId });
      return {
        isError: false,
        content: [{
          type: "text",
          text: JSON.stringify({
            thread: {
              id: "thread-1",
              title: "Project chat",
              createdAt: 10,
              updatedAt: 20,
            },
            page: { order: "newest_first", hasMore: false, nextCursor: null },
            turns: [{
              id: "turn-1",
              status: "completed",
              startedAt: 10,
              completedAt: 20,
              items: [
                {
                  type: "userMessage",
                  id: "user-1",
                  content: [{ type: "text", text: "question" }],
                },
                { type: "toolCall", id: "tool-1", name: "read" },
                { type: "toolResult", id: "tool-result-1", text: "large tool output" },
                { type: "agentMessage", id: "assistant-1", text: "answer" },
              ],
            }],
            attachments: [],
          }),
        }],
      };
    },
  };

  const transcript = await readCompleteThread(client, "thread-1", "context-thread");
  assert.equal(calls.length, 1);
  assert.equal(calls[0].name, "read_thread");
  assert.equal(calls[0].args.includeOutputs, true);
  assert.deepEqual(
    transcript.pages[0].messages.map((message) => [message.role, message.text]),
    [
      ["assistant", "answer"],
      ["user", "question"],
    ],
  );
});

test("NativeAppToolsClient uses the ChatGPT host framing and namespace contract", async () => {
  const socketPath = path.join(os.tmpdir(), `chat-history-app-tools-${process.pid}.sock`);
  fs.rmSync(socketPath, { force: true });
  const requests = [];
  const server = net.createServer((socket) => {
    let pending = Buffer.alloc(0);
    socket.on("data", (chunk) => {
      pending = Buffer.concat([pending, chunk]);
      while (pending.length >= 4) {
        const size = pending.readUInt32LE(0);
        if (pending.length < size + 4) return;
        const request = JSON.parse(pending.subarray(4, size + 4).toString("utf8"));
        pending = pending.subarray(size + 4);
        requests.push(request);
        if (request.method === "tools/list") {
          socket.write(encodeNativeFrame({
            jsonrpc: "2.0",
            id: request.id,
            result: {
              tools: [
                { name: "list_threads", namespace: "chatgpt", inputSchema: { type: "object" } },
              ],
            },
          }));
        } else if (request.method === "tools/call") {
          socket.write(encodeNativeFrame({
            jsonrpc: "2.0",
            id: request.id,
            result: {
              success: true,
              contentItems: [{ type: "inputText", text: JSON.stringify({ threads: [] }) }],
            },
          }));
        }
      }
    });
  });
  await new Promise((resolve, reject) => {
    server.once("error", reject);
    server.listen(socketPath, resolve);
  });
  const client = new NativeAppToolsClient(socketPath);
  try {
    await client.listTools();
    const result = await client.callTool("list_threads", { limit: 50 }, "context-thread");
    assert.equal(result.isError, false);
    assert.deepEqual(JSON.parse(result.content[0].text), { threads: [] });
    assert.equal(requests[0].method, "tools/list");
    assert.deepEqual(requests[0].params, { threadStartKind: "all" });
    assert.equal(requests[1].method, "tools/call");
    assert.equal(requests[1].params.callerSource, "codex");
    assert.equal(requests[1].params.namespace, "chatgpt");
    assert.equal(requests[1].params.threadId, "context-thread");
    assert.equal(requests[1].params.tool, "list_threads");
  } finally {
    client.close();
    await new Promise((resolve) => server.close(resolve));
    fs.rmSync(socketPath, { force: true });
  }
});
