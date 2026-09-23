#!/usr/bin/env node

import { spawn, spawnSync } from "node:child_process";
import { createHash, randomUUID } from "node:crypto";
import fs from "node:fs";
import net from "node:net";
import os from "node:os";
import path from "node:path";
import readline from "node:readline";
import { pathToFileURL } from "node:url";

const DATA_HOME = process.env.CHAT_HISTORY_DATA_HOME
  ?? path.join(os.homedir(), "Library/Application Support/chat-history-index-mcp");
const CLI = path.join(DATA_HOME, "bin/chat-history-cli");
const CODEX_DB = path.join(os.homedir(), ".codex/sqlite/codex-dev.db");
const DISCOVERY_LIMIT = 50;
const TURN_LIMIT = 10;
const MAX_MESSAGE_CHARS = 20_000;
const TRUNCATION_GUARD_CHARS = 19_990;
const MAX_PAGES = 1_000;
const REQUEST_TIMEOUT_MS = 30_000;
const DEFAULT_POLL_INTERVAL_MS = 120_000;
const MAX_NATIVE_FRAME_BYTES = 8 * 1024 * 1024;
const STATUS_PATH = path.join(DATA_HOME, "cache/chatgpt-live-collector-status.json");
const LOG_PATH = path.join(DATA_HOME, "logs/chatgpt-live-collector.log");
const ERROR_LOG_PATH = path.join(DATA_HOME, "logs/chatgpt-live-collector.error.log");
const DAEMON_LOCK = path.join(DATA_HOME, "cache/chatgpt-live-collector-daemon.lock");

function pipeIdentity(pipePath) {
  return createHash("sha256").update(pipePath).digest("hex").slice(0, 16);
}

class NativeAppToolsClient {
  constructor(pipePath) {
    this.pipePath = pipePath;
    this.nextId = 1;
    this.pending = new Map();
    this.pendingData = Buffer.alloc(0);
    this.socket = null;
    this.toolsByName = new Map();
    this.closed = false;
  }

  async connect() {
    if (this.socket != null && !this.socket.destroyed) return;
    await new Promise((resolve, reject) => {
      const socket = net.createConnection(this.pipePath);
      const fail = (error) => {
        socket.destroy();
        reject(error);
      };
      socket.once("error", fail);
      socket.once("connect", () => {
        socket.off("error", fail);
        this.socket = socket;
        socket.on("data", (chunk) => this.onData(chunk));
        socket.on("error", (error) => this.onDisconnect(error));
        socket.on("close", () => this.onDisconnect(new Error("ChatGPT App Tools pipe closed")));
        resolve();
      });
    });
  }

  onData(chunk) {
    this.pendingData = Buffer.concat([this.pendingData, chunk]);
    while (this.pendingData.length >= 4) {
      const frameBytes = this.pendingData.readUInt32LE(0);
      if (frameBytes <= 0 || frameBytes > MAX_NATIVE_FRAME_BYTES) {
        this.onDisconnect(new Error(`invalid App Tools frame size: ${frameBytes}`));
        return;
      }
      if (this.pendingData.length < frameBytes + 4) return;
      const frame = this.pendingData.subarray(4, frameBytes + 4);
      this.pendingData = this.pendingData.subarray(frameBytes + 4);
      let message;
      try {
        message = JSON.parse(frame.toString("utf8"));
      } catch {
        continue;
      }
      if (message.id == null) continue;
      const entry = this.pending.get(String(message.id));
      if (entry == null) continue;
      this.pending.delete(String(message.id));
      clearTimeout(entry.timer);
      if (message.error != null) {
        entry.reject(new Error(`App Tools ${message.error.code ?? "error"}: ${message.error.message ?? "unknown error"}`));
      } else {
        entry.resolve(message.result);
      }
    }
  }

  onDisconnect(error) {
    if (this.closed) return;
    this.closed = true;
    this.socket?.destroy();
    this.socket = null;
    for (const entry of this.pending.values()) {
      clearTimeout(entry.timer);
      entry.reject(error);
    }
    this.pending.clear();
  }

  async request(method, params = {}, timeoutMs = REQUEST_TIMEOUT_MS) {
    if (this.closed) throw new Error("App Tools client is closed");
    await this.connect();
    if (this.socket == null) throw new Error("App Tools client is not connected");
    const id = this.nextId++;
    const payload = Buffer.from(JSON.stringify({ jsonrpc: "2.0", id, method, params }), "utf8");
    const frame = Buffer.allocUnsafe(payload.length + 4);
    frame.writeUInt32LE(payload.length, 0);
    payload.copy(frame, 4);
    return new Promise((resolve, reject) => {
      const timer = setTimeout(() => {
        this.pending.delete(String(id));
        reject(new Error(`App Tools request timed out: ${method}`));
      }, timeoutMs);
      this.pending.set(String(id), { resolve, reject, timer });
      this.socket.write(frame, (error) => {
        if (error != null) {
          clearTimeout(timer);
          this.pending.delete(String(id));
          reject(error);
        }
      });
    });
  }

  async listTools() {
    const result = await this.request("tools/list", { threadStartKind: "all" });
    this.toolsByName = new Map((result.tools ?? []).map((tool) => [tool.name, tool]));
    return result;
  }

  async callTool(name, args, contextThreadId) {
    if (this.toolsByName.size === 0) await this.listTools();
    const tool = this.toolsByName.get(name);
    if (tool == null) throw new Error(`ChatGPT App Tools does not expose ${name}`);
    const result = await this.request("tools/call", {
      arguments: args,
      callId: `chat-history-${randomUUID()}`,
      namespace: tool.namespace,
      threadId: contextThreadId,
      tool: name,
      turnId: `chat-history-${randomUUID()}`,
    }, 60_000);
    return {
      isError: result.success !== true,
      content: (result.contentItems ?? []).map((item) => {
        if (item.type === "inputText") return { type: "text", text: item.text };
        return { type: "text", text: item.imageUrl ?? item.audioUrl ?? "" };
      }),
    };
  }

  close() {
    if (this.closed) return;
    this.closed = true;
    this.socket?.destroy();
    this.socket = null;
  }
}

function mustExist(file) {
  if (!fs.existsSync(file)) throw new Error(`required file is missing: ${file}`);
}

function runJson(command, args, input = null, extraEnv = {}) {
  const result = spawnSync(command, args, {
    input,
    encoding: "utf8",
    maxBuffer: 128 * 1024 * 1024,
    env: { ...process.env, CHAT_HISTORY_DATA_HOME: DATA_HOME, ...extraEnv },
  });
  if (result.status !== 0) {
    throw new Error(`${path.basename(command)} ${args.join(" ")} failed: ${(result.stderr || result.stdout).trim()}`);
  }
  return JSON.parse(result.stdout);
}

function cliJson(args, input = null) {
  return runJson(CLI, args, input);
}

function selectContextThread() {
  if (!fs.existsSync(CODEX_DB)) return null;
  const sql = [
    "pragma query_only=on;",
    "select thread_id from local_thread_catalog",
    "where host_id='local' and source_kind!='chatgpt' and missing_candidate=0",
    "order by source_recency_at desc, source_created_at desc limit 1;",
  ].join(" ");
  const result = spawnSync("/usr/bin/sqlite3", [CODEX_DB, sql], {
    encoding: "utf8",
    timeout: 5_000,
  });
  if (result.status !== 0) return null;
  return result.stdout.trim() || null;
}

function toolText(result) {
  if (result?.isError === true) {
    const message = (result.content ?? []).filter((item) => item.type === "text").map((item) => item.text).join("\n");
    throw new Error(message || "ChatGPT App Tool returned an error");
  }
  const text = (result?.content ?? []).filter((item) => item.type === "text").map((item) => item.text).join("\n");
  if (!text) throw new Error("ChatGPT App Tool returned no text payload");
  return JSON.parse(text);
}

function bridgeThread(entry) {
  return {
    thread_id: entry.id,
    kind: entry.kind,
    title: entry.title ?? "",
    create_time: entry.createdAt ?? null,
    update_time: entry.updatedAt ?? null,
  };
}

function messageText(item) {
  if (item.type === "agentMessage") return item.text ?? "";
  if (item.type === "userMessage") {
    return (item.content ?? [])
      .filter((content) => content.type === "text")
      .map((content) => content.text ?? "")
      .join("\n");
  }
  return null;
}

function bridgeMessages(turn) {
  const result = [];
  // The bridge returns turns newest-first but messages inside a turn oldest-first.
  // Reverse the items here because the Rust importer reverses the complete stream once.
  for (const [reverseIndex, item] of [...(turn.items ?? [])].reverse().entries()) {
    const text = messageText(item);
    if (text == null) continue;
    const role = item.type === "userMessage" ? "user" : "assistant";
    const originalIndex = (turn.items?.length ?? 0) - reverseIndex - 1;
    const messageId = item.id ?? `${turn.id}:${role}:${originalIndex}`;
    const truncated = text.length >= TRUNCATION_GUARD_CHARS;
    result.push({
      message_id: messageId,
      role,
      create_time: role === "user"
        ? (turn.startedAt ?? null)
        : (turn.completedAt ?? turn.startedAt ?? null),
      text,
      truncated,
      inaccessible: false,
      raw: {
        turn_id: turn.id,
        turn_status: turn.status ?? null,
        item,
      },
    });
  }
  return result;
}

class PermanentIncompleteError extends Error {}

async function readCompleteThread(client, threadId, contextThreadId) {
  let cursor = null;
  const seenCursors = new Set();
  const pages = [];
  let threadMetadata = null;
  let attachments = [];

  for (let pageIndex = 0; pageIndex < MAX_PAGES; pageIndex += 1) {
    const args = {
      threadId,
      turnLimit: TURN_LIMIT,
      includeOutputs: false,
      maxOutputCharsPerItem: MAX_MESSAGE_CHARS,
    };
    if (cursor != null) args.cursor = cursor;
    const payload = toolText(await client.callTool("read_thread", args, contextThreadId));
    if (payload.page?.order !== "newest_first") {
      throw new PermanentIncompleteError(`unexpected read_thread order: ${payload.page?.order ?? "missing"}`);
    }
    if (threadMetadata == null) threadMetadata = payload.thread ?? null;
    if (Array.isArray(payload.attachments) && payload.attachments.length > 0) {
      attachments = payload.attachments;
    }
    const messages = (payload.turns ?? []).flatMap((turn) => bridgeMessages(turn));
    if (messages.some((message) => message.truncated)) {
      throw new PermanentIncompleteError(`read_thread reached the ${MAX_MESSAGE_CHARS}-character per-message safety limit`);
    }
    const hasMore = payload.page?.hasMore === true;
    const nextCursor = payload.page?.nextCursor ?? null;
    pages.push({
      request_cursor: cursor,
      next_cursor: nextCursor,
      has_more: hasMore,
      messages,
    });
    if (!hasMore) break;
    if (typeof nextCursor !== "string" || nextCursor.length === 0) {
      throw new PermanentIncompleteError("read_thread reported hasMore without nextCursor");
    }
    if (seenCursors.has(nextCursor)) {
      throw new PermanentIncompleteError("read_thread cursor loop detected");
    }
    seenCursors.add(nextCursor);
    cursor = nextCursor;
  }

  if (pages.length === 0 || pages.at(-1)?.has_more === true) {
    throw new PermanentIncompleteError(`read_thread exceeded ${MAX_PAGES} pages`);
  }
  if (threadMetadata == null) {
    throw new PermanentIncompleteError("read_thread returned no thread metadata");
  }
  return {
    thread_id: threadId,
    title: threadMetadata.title ?? "",
    create_time: threadMetadata.createdAt ?? null,
    update_time: threadMetadata.updatedAt ?? null,
    model: null,
    source_url: null,
    attachment_metadata: attachments,
    pages,
  };
}

function importTranscript(transcript) {
  const input = JSON.stringify(transcript);
  return cliJson([
    "chatgpt-import-thread",
    "--path", "-",
    "--stdin-bytes", String(Buffer.byteLength(input)),
  ], input);
}

function markBlocked(threadId, reason) {
  return cliJson(["chatgpt-block", threadId, "--reason", reason]);
}

function isRateLimit(error) {
  return /too many requests|rate.?limit|429/iu.test(String(error?.message ?? error));
}

function acquireLock() {
  const lock = path.join(DATA_HOME, "cache/chatgpt-live-collector.lock");
  fs.mkdirSync(path.dirname(lock), { recursive: true });
  for (let attempt = 0; attempt < 2; attempt += 1) {
    try {
      fs.mkdirSync(lock);
      fs.writeFileSync(path.join(lock, "pid"), `${process.pid}\n`, { mode: 0o600 });
      return () => fs.rmSync(lock, { recursive: true, force: true });
    } catch (error) {
      if (error?.code !== "EEXIST") throw error;
      let existingPid = null;
      try {
        existingPid = Number(fs.readFileSync(path.join(lock, "pid"), "utf8").trim());
      } catch {}
      let alive = false;
      if (Number.isInteger(existingPid) && existingPid > 0) {
        try {
          process.kill(existingPid, 0);
          alive = true;
        } catch (probeError) {
          alive = probeError?.code !== "ESRCH";
        }
      }
      if (alive) return null;
      fs.rmSync(lock, { recursive: true, force: true });
    }
  }
  return null;
}

function appendLog(file, payload) {
  fs.mkdirSync(path.dirname(file), { recursive: true });
  fs.appendFileSync(file, `${JSON.stringify({ at: new Date().toISOString(), ...payload })}\n`, { mode: 0o600 });
}

function writeStatus(payload) {
  fs.mkdirSync(path.dirname(STATUS_PATH), { recursive: true });
  const staged = `${STATUS_PATH}.${process.pid}.tmp`;
  fs.writeFileSync(staged, `${JSON.stringify(payload, null, 2)}\n`, { mode: 0o600 });
  fs.renameSync(staged, STATUS_PATH);
}

function acquirePidLock(lock) {
  fs.mkdirSync(path.dirname(lock), { recursive: true });
  for (let attempt = 0; attempt < 2; attempt += 1) {
    try {
      fs.mkdirSync(lock);
      fs.writeFileSync(path.join(lock, "pid"), `${process.pid}\n`, { mode: 0o600 });
      return () => fs.rmSync(lock, { recursive: true, force: true });
    } catch (error) {
      if (error?.code !== "EEXIST") throw error;
      let existingPid = null;
      try {
        existingPid = Number(fs.readFileSync(path.join(lock, "pid"), "utf8").trim());
      } catch {}
      let alive = false;
      if (Number.isInteger(existingPid) && existingPid > 0) {
        try {
          process.kill(existingPid, 0);
          alive = true;
        } catch (probeError) {
          alive = probeError?.code !== "ESRCH";
        }
      }
      if (alive) return null;
      fs.rmSync(lock, { recursive: true, force: true });
    }
  }
  return null;
}

async function syncWithClient(client, contextThreadId) {
  mustExist(CLI);
  const releaseLock = acquireLock();
  if (releaseLock == null) return { event: "chatgpt_live_sync_skipped", reason: "locked" };
  try {
    const catalog = toolText(await client.callTool("list_threads", { limit: DISCOVERY_LIMIT }, contextThreadId));
    const statusById = new Map();
    for (const entry of [...(catalog.threads ?? []), ...(catalog.pinnedThreads ?? [])]) {
      statusById.set(entry.id, entry.status ?? null);
    }
    const snapshot = {
      requested_limit: DISCOVERY_LIMIT,
      threads: (catalog.threads ?? []).map(bridgeThread),
      pinned_threads: (catalog.pinnedThreads ?? []).map(bridgeThread),
    };
    const snapshotText = JSON.stringify(snapshot);
    const planned = cliJson([
      "chatgpt-plan-recent",
      "--path", "-",
      "--stdin-bytes", String(Buffer.byteLength(snapshotText)),
    ], snapshotText);
    const selected = planned.plan?.selected ?? [];
    if (selected.length === 0) {
      return { event: "chatgpt_live_sync", imported: 0, blocked: 0, deferred_active: 0, titles: [] };
    }

    let imported = 0;
    let deferredActive = 0;
    let blocked = 0;
    const importedTitles = [];
    for (const pending of selected) {
      const status = statusById.get(pending.thread_id);
      if (status !== "idle") {
        deferredActive += 1;
        continue;
      }
      try {
        const transcript = await readCompleteThread(client, pending.thread_id, contextThreadId);
        importTranscript(transcript);
        imported += 1;
        importedTitles.push(transcript.title);
      } catch (error) {
        if (isRateLimit(error)) {
          return { event: "chatgpt_live_rate_limited", imported, pending: pending.thread_id };
        }
        if (error instanceof PermanentIncompleteError) {
          markBlocked(pending.thread_id, error.message);
          blocked += 1;
          continue;
        }
        appendLog(ERROR_LOG_PATH, {
          event: "chatgpt_live_thread_error",
          thread_id: pending.thread_id,
          error: String(error?.message ?? error),
        });
        // Leave transient failures pending and do not advance the safe cursor.
      }
    }
    return {
      event: "chatgpt_live_sync",
      imported,
      blocked,
      deferred_active: deferredActive,
      titles: importedTitles,
    };
  } finally {
    releaseLock();
  }
}

async function syncOnce() {
  const contextThreadId = selectContextThread();
  if (contextThreadId == null) return { event: "chatgpt_live_sync_skipped", reason: "no-context-thread" };
  const pipePath = process.env.CODEX_APP_TOOLS_PIPE_PATH?.trim();
  if (!pipePath) return { event: "chatgpt_live_sync_skipped", reason: "no-app-tools-pipe" };
  const client = new NativeAppToolsClient(pipePath);
  try {
    const listed = await client.listTools();
    const names = new Set((listed.tools ?? []).map((tool) => tool.name));
    if (!names.has("list_threads") || !names.has("read_thread")) {
      throw new Error("ChatGPT App Tools does not expose list_threads/read_thread");
    }
    return await syncWithClient(client, contextThreadId);
  } finally {
    client.close();
  }
}

async function daemonLoop() {
  const releaseDaemonLock = acquirePidLock(DAEMON_LOCK);
  if (releaseDaemonLock == null) return;
  const interval = Math.max(
    30_000,
    Number(process.env.CHAT_HISTORY_CHATGPT_POLL_INTERVAL_MS ?? DEFAULT_POLL_INTERVAL_MS) || DEFAULT_POLL_INTERVAL_MS,
  );
  const pipePath = process.env.CODEX_APP_TOOLS_PIPE_PATH?.trim();
  if (!pipePath) {
    writeStatus({ state: "degraded", pid: process.pid, pipe_present: false, error: "no-app-tools-pipe" });
    releaseDaemonLock();
    return;
  }
  const client = new NativeAppToolsClient(pipePath);
  const currentPipeIdentity = pipeIdentity(pipePath);
  let stopping = false;
  let wakeSleep = null;
  const stop = () => {
    stopping = true;
    client.close();
    wakeSleep?.();
  };
  process.once("SIGTERM", stop);
  process.once("SIGINT", stop);
  try {
    const listed = await client.listTools();
    const names = new Set((listed.tools ?? []).map((tool) => tool.name));
    if (!names.has("list_threads") || !names.has("read_thread")) {
      throw new Error("ChatGPT App Tools does not expose list_threads/read_thread");
    }
    writeStatus({
      state: "running",
      pid: process.pid,
      started_at: new Date().toISOString(),
      pipe_present: true,
      pipe_connected: true,
      pipe_identity: currentPipeIdentity,
      interval_ms: interval,
    });
    while (!stopping) {
      const contextThreadId = selectContextThread();
      const result = contextThreadId == null
        ? { event: "chatgpt_live_sync_skipped", reason: "no-context-thread" }
        : await syncWithClient(client, contextThreadId);
      const status = {
        state: "running",
        pid: process.pid,
        checked_at: new Date().toISOString(),
        pipe_present: true,
        pipe_connected: !client.closed,
        pipe_identity: currentPipeIdentity,
        interval_ms: interval,
        last_result: result,
      };
      writeStatus(status);
      if (result.imported > 0 || result.blocked > 0 || result.event !== "chatgpt_live_sync") {
        appendLog(LOG_PATH, result);
      }
      if (client.closed) throw new Error("ChatGPT App Tools pipe disconnected");
      await new Promise((resolve) => {
        let settled = false;
        const finish = () => {
          if (settled) return;
          settled = true;
          clearTimeout(timer);
          wakeSleep = null;
          resolve();
        };
        const timer = setTimeout(finish, interval);
        wakeSleep = finish;
        if (stopping) finish();
      });
    }
  } catch (error) {
    const detail = String(error?.stack ?? error);
    writeStatus({
      state: "degraded",
      pid: process.pid,
      checked_at: new Date().toISOString(),
      pipe_present: true,
      pipe_connected: false,
      pipe_identity: currentPipeIdentity,
      error: detail,
    });
    appendLog(ERROR_LOG_PATH, { event: "chatgpt_live_collector_error", error: detail });
  } finally {
    client.close();
    releaseDaemonLock();
  }
}

function pidAlive(pid) {
  if (!Number.isInteger(pid) || pid <= 0) return false;
  try {
    process.kill(pid, 0);
    return true;
  } catch (error) {
    return error?.code !== "ESRCH";
  }
}

async function ensureDaemonReady() {
  const pipePath = process.env.CODEX_APP_TOOLS_PIPE_PATH?.trim();
  if (!pipePath) throw new Error("CODEX_APP_TOOLS_PIPE_PATH is not available to the collector bootstrap");
  const expectedPipeIdentity = pipeIdentity(pipePath);
  try {
    const pid = Number(fs.readFileSync(path.join(DAEMON_LOCK, "pid"), "utf8").trim());
    if (pidAlive(pid)) {
      let status = null;
      try {
        status = JSON.parse(fs.readFileSync(STATUS_PATH, "utf8"));
      } catch {}
      if (
        status?.pid === pid
        && status?.state === "running"
        && status?.pipe_connected === true
        && status?.pipe_identity === expectedPipeIdentity
      ) {
        return pid;
      }
      try {
        process.kill(pid, "SIGTERM");
      } catch {}
      const deadline = Date.now() + 2_000;
      while (Date.now() < deadline && pidAlive(pid)) {
        await new Promise((resolve) => setTimeout(resolve, 50));
      }
    }
  } catch {}
  fs.rmSync(DAEMON_LOCK, { recursive: true, force: true });
  const child = spawn(process.execPath, [process.argv[1], "--daemon"], {
    detached: true,
    stdio: "ignore",
    env: { ...process.env },
  });
  child.unref();
  const deadline = Date.now() + 8_000;
  while (Date.now() < deadline) {
    let status = null;
    try {
      status = JSON.parse(fs.readFileSync(STATUS_PATH, "utf8"));
    } catch (error) {
      if (error instanceof SyntaxError) throw error;
    }
    if (status?.pid === child.pid && status.pipe_connected === true && status.state === "running") {
      return child.pid;
    }
    if (status?.pid === child.pid && status.state === "degraded") {
      throw new Error(status.error ?? "collector daemon failed to connect");
    }
    await new Promise((resolve) => setTimeout(resolve, 100));
  }
  throw new Error(`collector daemon ${child.pid} did not become ready`);
}

function runMcpSidecar() {
  const input = readline.createInterface({ input: process.stdin, crlfDelay: Infinity });
  input.on("line", async (line) => {
    let message;
    try {
      message = JSON.parse(line);
    } catch {
      return;
    }
    if (message.method === "notifications/initialized") return;
    if (message.id == null) return;
    const respond = (result) => process.stdout.write(`${JSON.stringify({ jsonrpc: "2.0", id: message.id, result })}\n`);
    if (message.method === "initialize") {
      try {
        await ensureDaemonReady();
        respond({
          protocolVersion: message.params?.protocolVersion ?? "2025-06-18",
          capabilities: { tools: {} },
          serverInfo: { name: "chatgpt-live-collector", version: "1.0.0" },
        });
      } catch (error) {
        appendLog(ERROR_LOG_PATH, {
          event: "chatgpt_live_collector_bootstrap_error",
          error: String(error?.stack ?? error),
        });
        process.stdout.write(`${JSON.stringify({
          jsonrpc: "2.0",
          id: message.id,
          error: { code: -32603, message: "ChatGPT live collector daemon failed to start" },
        })}\n`);
      }
      return;
    }
    if (message.method === "ping") {
      respond({});
      return;
    }
    if (message.method === "tools/list") {
      respond({
        tools: [
          {
            name: "chatgpt_live_collector_status",
            description: "Read the local ChatGPT live collector status. This tool does not trigger a sync.",
            inputSchema: { type: "object", properties: {}, additionalProperties: false },
            annotations: {
              title: "ChatGPT Live Collector Status",
              readOnlyHint: true,
              destructiveHint: false,
              openWorldHint: false,
            },
          },
        ],
      });
      return;
    }
    if (message.method === "tools/call") {
      if (message.params?.name !== "chatgpt_live_collector_status") {
        process.stdout.write(`${JSON.stringify({
          jsonrpc: "2.0",
          id: message.id,
          error: { code: -32602, message: `Unknown tool: ${message.params?.name ?? "missing"}` },
        })}\n`);
        return;
      }
      let status = { state: "starting", pid: process.pid };
      try {
        status = JSON.parse(fs.readFileSync(STATUS_PATH, "utf8"));
      } catch {}
      respond({
        content: [{ type: "text", text: JSON.stringify(status) }],
        structuredContent: status,
        isError: false,
      });
      return;
    }
    if (message.method === "resources/list") {
      respond({ resources: [] });
      return;
    }
    process.stdout.write(`${JSON.stringify({
      jsonrpc: "2.0",
      id: message.id,
      error: { code: -32601, message: `Method not found: ${message.method}` },
    })}\n`);
  });
}

if (process.argv[1] != null && import.meta.url === pathToFileURL(process.argv[1]).href) {
  if (process.argv.includes("--daemon")) {
    daemonLoop().catch((error) => {
      appendLog(ERROR_LOG_PATH, { event: "chatgpt_live_collector_fatal", error: String(error?.stack ?? error) });
      process.exitCode = 1;
    });
  } else if (process.argv.includes("--mcp-sidecar")) {
    runMcpSidecar();
  } else {
    syncOnce()
      .then((result) => {
        if (result.imported > 0 || result.blocked > 0 || result.event !== "chatgpt_live_sync") {
          console.log(JSON.stringify(result));
        }
      })
      .catch((error) => {
        console.error(JSON.stringify({ event: "chatgpt_live_collector_error", error: String(error?.stack ?? error) }));
        process.exitCode = 1;
      });
  }
}

export {
  NativeAppToolsClient,
  bridgeMessages,
  bridgeThread,
  ensureDaemonReady,
  messageText,
  readCompleteThread,
  syncWithClient,
  syncOnce,
};
