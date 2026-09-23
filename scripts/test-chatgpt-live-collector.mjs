import assert from "node:assert/strict";
import fs from "node:fs";
import net from "node:net";
import os from "node:os";
import path from "node:path";
import test from "node:test";

import {
  NativeAppToolsClient,
  bridgeMessages,
  bridgeThread,
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
    assert.equal(requests[1].params.namespace, "chatgpt");
    assert.equal(requests[1].params.threadId, "context-thread");
    assert.equal(requests[1].params.tool, "list_threads");
  } finally {
    client.close();
    await new Promise((resolve) => server.close(resolve));
    fs.rmSync(socketPath, { force: true });
  }
});
