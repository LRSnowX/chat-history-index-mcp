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
const MEMORY_COMPILER_LOCK = path.join(DATA_HOME, "cache/memory-auto-compiler.lock");
const MEMORY_COMPILER_STATUS_PATH = path.join(DATA_HOME, "cache/memory-auto-compiler-status.json");
const MEMORY_COMPILER_HISTORY_PATH = path.join(DATA_HOME, "cache/memory-auto-compiler-history.json");
const MEMORY_COMPILER_HISTORY_LIMIT = 20;
const MEMORY_COMPILER_STATUS_RECENT_RUNS = 5;
const DEFAULT_MEMORY_AUTO_SCAN_LIMIT = 500;
const DEFAULT_MEMORY_AUTO_MAX_CONVERSATIONS = 1;
const DEFAULT_MEMORY_AUTO_MAX_MESSAGES = 8;
const DEFAULT_MEMORY_AUTO_MAX_PENDING_CANDIDATES = 20;
const MAX_MEMORY_CANDIDATES_PER_MODEL_ATTEMPT = 8;
const MEMORY_AUTO_TRIGGER_ENV = "CHAT_HISTORY_MEMORY_AUTO_TRIGGER_CONVERSATIONS";

function pipeIdentity(pipePath) {
  return createHash("sha256").update(pipePath).digest("hex").slice(0, 16);
}

function classifyRpcError(code, message) {
  const text = String(message ?? "");
  if (/too many requests|rate.?limit|\b429\b/iu.test(text)) return "rate_limited";
  if (/not found|does not exist|unknown thread|missing thread/iu.test(text)) return "not_found";
  if (/archiv|unavailable|cannot be read|not accessible|inaccessible/iu.test(text)) return "unavailable";
  if (/invalid.*argument|invalid.*request|bad request|malformed/iu.test(text)) return "invalid_arguments";
  if (/permission|forbidden|unauthorized|access denied/iu.test(text)) return "permission";
  if (/internal|server error|temporar|try again|unavailable service/iu.test(text)) return "internal";
  if (code === -32602) return "invalid_arguments";
  if (code === -32601) return "not_found";
  if (code === -32603) return "internal";
  return "unknown";
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
        const error = new Error("App Tools RPC request failed");
        error.appToolsRpcCode =
          typeof message.error.code === "number" || typeof message.error.code === "string"
            ? String(message.error.code).slice(0, 32)
            : null;
        error.appToolsRpcCategory = classifyRpcError(message.error.code, message.error.message);
        entry.reject(error);
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
      callerSource: "codex",
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
    const message = (result.content ?? [])
      .filter((item) => item.type === "text")
      .map((item) => item.text)
      .join("\n");
    const error = new Error("ChatGPT App Tool returned an error");
    error.toolResultCategory = classifyToolResultError(message);
    throw error;
  }
  const text = (result?.content ?? []).filter((item) => item.type === "text").map((item) => item.text).join("\n");
  if (!text) {
    const error = new Error("ChatGPT App Tool returned no text payload");
    error.toolPayloadCategory = "empty";
    throw error;
  }
  try {
    return JSON.parse(text);
  } catch {
    const error = new Error("ChatGPT App Tool returned invalid JSON");
    error.toolPayloadCategory = "invalid_json";
    throw error;
  }
}

function classifyToolResultError(message) {
  const text = String(message ?? "");
  if (/too many requests|rate.?limit|\b429\b/iu.test(text)) return "rate_limited";
  if (/not found|does not exist|unknown thread|missing thread/iu.test(text)) return "not_found";
  if (/archiv|unavailable|cannot be read|not accessible|inaccessible/iu.test(text)) return "unavailable";
  if (/invalid.*argument|invalid.*request|bad request|malformed/iu.test(text)) return "invalid_arguments";
  if (/permission|forbidden|unauthorized|access denied/iu.test(text)) return "permission";
  if (/internal|server error|temporar|try again|unavailable service/iu.test(text)) return "internal";
  return "unknown";
}

function bridgeThread(entry, observedAt = null) {
  return {
    thread_id: entry.id,
    kind: entry.kind,
    title: entry.title ?? "",
    create_time: entry.createdAt ?? null,
    update_time: entry.updatedAt ?? null,
    status: typeof entry.status === "string" ? entry.status : null,
    observed_at: Number.isFinite(observedAt) ? observedAt : null,
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
      stable_identity: typeof item.id === "string" && item.id.length > 0,
      raw: {
        turn_id: turn.id,
        turn_status: turn.status ?? null,
        item,
      },
    });
  }
  return result;
}

class PermanentIncompleteError extends Error {
  constructor(message, repairCode = "REPLAY_INCOMPLETE", verificationDetail = null) {
    super(message);
    this.repairCode = repairCode;
    this.verificationDetail = verificationDetail;
  }
}

const REPAIR_FAILURE_CODES = new Set([
  "TRUNCATED_NEW_TAIL", "MISSING_OVERLAP", "PREFIX_DIVERGENCE", "AMBIGUOUS_IDENTITY",
  "PROVIDER_CHANGED", "REPLAY_INCOMPLETE", "BASELINE_CHANGED", "INVALID_EXPORT",
  "EXPORT_IDENTITY", "STALE_EXPORT", "LIVE_BLOCK_REQUIRED", "PUBLICATION_FAILED",
]);

function repairFailureCode(error) {
  const code = error?.repairCode ?? /CHIM_REPAIR_([A-Z_]+):/u.exec(String(error?.message ?? ""))?.[1];
  return REPAIR_FAILURE_CODES.has(code) ? code : "REPLAY_INCOMPLETE";
}

function verificationErrorClass(error) {
  const message = String(error?.message ?? "");
  if (typeof error?.appToolsRpcCategory === "string") {
    return `app_tools_rpc_${error.appToolsRpcCategory}`;
  }
  if (typeof error?.toolResultCategory === "string") {
    return `tool_result_${error.toolResultCategory}`;
  }
  if (typeof error?.toolPayloadCategory === "string") {
    return `tool_payload_${error.toolPayloadCategory}`;
  }
  if (error instanceof PermanentIncompleteError) {
    return `permanent_${String(error.repairCode ?? "replay_incomplete").toLowerCase()}`;
  }
  if (/App Tools request timed out:/iu.test(message)) return "app_tools_timeout";
  if (/^App Tools [^:]+:/iu.test(message)) return "app_tools_rpc_error";
  if (/ChatGPT App Tool returned no text payload/iu.test(message)) return "tool_result_empty";
  if (/ChatGPT App Tool returned an error/iu.test(message)) return "tool_result_error";
  if (/not found|does not exist|unknown thread/iu.test(message)) return "tool_result_not_found";
  if (/archiv/iu.test(message)) return "tool_result_archived";
  if (/permission|forbidden|unauthorized/iu.test(message)) return "tool_result_permission";
  if (/too many requests|rate.?limit|429/iu.test(message)) return "tool_result_rate_limited";
  if (/Unexpected token|JSON|parse/iu.test(message)) return "tool_payload_invalid_json";
  if (/pipe closed|ECONN|socket|client is closed/iu.test(message)) return "app_tools_transport";
  if (/chat-history-cli .* failed:/iu.test(message)) return "cli_failure";
  return "unknown";
}

function verificationDetailClass(error) {
  const detail = error?.verificationDetail;
  return new Set([
    "thread_id_mismatch",
    "revision_mismatch",
    "status_mismatch",
    "missing_thread_metadata",
  ]).has(detail) ? detail : null;
}

function recordRepairFailure(threadId, error) {
  const code = repairFailureCode(error);
  const fallback = { code, reason: "Live repair rejected; inspect durable collector diagnostics" };
  try {
    const result = cliJson(["chatgpt-record-repair-failure", threadId, "--code", code]);
    return { ...fallback, diagnostic_persisted: result.recorded === true };
  } catch {
    // Never echo raw subprocess/provider failures, which may contain transcript text.
  }
  return { ...fallback, diagnostic_persisted: false };
}

async function readCompleteThread(client, threadId, contextThreadId, continuationRevision) {
  let cursor = null;
  const seenCursors = new Set();
  const pages = [];
  let threadMetadata = null;
  let attachments = [];

  for (let pageIndex = 0; pageIndex < MAX_PAGES; pageIndex += 1) {
    const args = {
      threadId,
      turnLimit: TURN_LIMIT,
      // Assistant responses are output items in the native ChatGPT thread bridge.
      // Request them, then let bridgeMessages keep only userMessage/agentMessage
      // while ignoring tool calls, tool outputs, and other non-conversation items.
      includeOutputs: true,
      maxOutputCharsPerItem: MAX_MESSAGE_CHARS,
    };
    if (cursor != null) args.cursor = cursor;
    const payload = toolText(await client.callTool("read_thread", args, contextThreadId));
    if (payload.page?.order !== "newest_first") {
      throw new PermanentIncompleteError(`unexpected read_thread order: ${payload.page?.order ?? "missing"}`);
    }
    if (continuationRevision !== undefined) {
      if (payload.thread?.id !== threadId) {
        throw new PermanentIncompleteError(
          "provider thread identity changed during continuation replay",
          "PROVIDER_CHANGED",
          "thread_id_mismatch",
        );
      }
      if (payload.thread?.updatedAt !== continuationRevision) {
        throw new PermanentIncompleteError(
          "provider revision changed during continuation replay",
          "PROVIDER_CHANGED",
          "revision_mismatch",
        );
      }
      if (payload.thread?.status != null && payload.thread.status !== "idle") {
        throw new PermanentIncompleteError(
          "provider status changed during continuation replay",
          "PROVIDER_CHANGED",
          "status_mismatch",
        );
      }
    }
    if (threadMetadata == null) threadMetadata = payload.thread ?? null;
    if (Array.isArray(payload.attachments) && payload.attachments.length > 0) {
      attachments = payload.attachments;
    }
    const messages = (payload.turns ?? []).flatMap((turn) => bridgeMessages(turn));
    if (continuationRevision === undefined && messages.some((message) => message.truncated)) {
      throw new PermanentIncompleteError(`read_thread reached the ${MAX_MESSAGE_CHARS}-character per-message safety limit`);
    }
    const hasMore = payload.page?.hasMore === true;
    const nextCursor = payload.page?.nextCursor ?? null;
    pages.push({
      request_cursor: cursor,
      next_cursor: nextCursor,
      has_more: hasMore,
      messages,
      provider_revision: Number.isFinite(payload.thread?.updatedAt) ? payload.thread.updatedAt : null,
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

function pendingHistoricalRestores(limit = 16) {
  const result = cliJson(["chatgpt-history-restore-pending", "--limit", String(limit)]);
  return Array.isArray(result.pending) ? result.pending : [];
}

function priorRateLimitedThread() {
  try {
    const status = JSON.parse(fs.readFileSync(STATUS_PATH, "utf8"));
    const result = status?.last_result;
    return result?.event === "chatgpt_live_rate_limited"
      && typeof result.pending === "string"
      && result.pending.length <= 256
      ? result.pending
      : null;
  } catch {
    return null;
  }
}

function rotateAfterThread(entries, threadId) {
  if (!threadId) return entries;
  const index = entries.findIndex((entry) => entry.thread_id === threadId);
  if (index < 0) return entries;
  return [...entries.slice(index + 1), ...entries.slice(0, index + 1)];
}

function historicalRestoreCandidate(restores, previousRateLimited) {
  return rotateAfterThread(
    restores.map((restore) => ({
      thread_id: restore.conversation_id,
      title: restore.title,
      update_time: restore.update_time,
      continuation_repair: true,
      historical_restore_verification: true,
    })),
    previousRateLimited,
  )[0] ?? null;
}

function buildSelectedQueue(ordinarySelected, repairIds, previousRateLimited) {
  const selected = [];
  const selectedIds = new Set();
  const repairEntries = rotateAfterThread(
    repairIds.map((threadId) => ({ thread_id: threadId, continuation_repair: true })),
    previousRateLimited,
  );
  for (const repair of repairEntries) {
    if (selectedIds.has(repair.thread_id)) continue;
    selected.push(repair);
    selectedIds.add(repair.thread_id);
  }
  for (const ordinary of rotateAfterThread([...ordinarySelected], previousRateLimited)) {
    if (selectedIds.has(ordinary.thread_id)) continue;
    selected.push(ordinary);
    selectedIds.add(ordinary.thread_id);
  }
  return selected;
}

function recordProviderObservation(thread) {
  const input = JSON.stringify(thread);
  return cliJson([
    "chatgpt-observe-thread",
    "--path", "-",
    "--stdin-bytes", String(Buffer.byteLength(input)),
  ], input);
}

function verificationStage(error, stage) {
  if (error != null && typeof error === "object" && error.verificationStage == null) {
    error.verificationStage = stage;
  }
  return error;
}

async function atVerificationStage(stage, operation) {
  try {
    return await operation();
  } catch (error) {
    throw verificationStage(error, stage);
  }
}

function atVerificationStageSync(stage, operation) {
  try {
    return operation();
  } catch (error) {
    throw verificationStage(error, stage);
  }
}

async function observeListThread(client, threadId, contextThreadId) {
  const catalog = toolText(await client.callTool(
    "list_threads",
    { limit: DISCOVERY_LIMIT },
    contextThreadId,
  ));
  const entries = [...(catalog.threads ?? []), ...(catalog.pinnedThreads ?? [])]
    .filter((entry) => entry.id === threadId);
  if (
    entries.length === 0
    || entries.some((entry) => (
      entry.kind !== "chatgpt"
      || entry.status !== "idle"
      || !Number.isFinite(entry.updatedAt)
      || entry.updatedAt !== entries[0].updatedAt
    ))
  ) {
    throw new PermanentIncompleteError(
      "discovery provider is absent, contradictory or non-idle",
      "PROVIDER_CHANGED",
    );
  }
  return bridgeThread(entries[0], Date.now() / 1000);
}

async function observeReadThread(
  client,
  threadId,
  contextThreadId,
  fallbackProviderState,
  allowMissingStatus = false,
) {
  const payload = toolText(await client.callTool("read_thread", {
    threadId,
    turnLimit: 1,
    includeOutputs: false,
    maxOutputCharsPerItem: 256,
  }, contextThreadId));
  const metadata = payload.thread ?? {};
  if (metadata.id !== threadId || !Number.isFinite(metadata.updatedAt)) {
    throw new PermanentIncompleteError(
      "read_thread provider identity or revision is unavailable",
      "PROVIDER_CHANGED",
    );
  }
  const directStatus = typeof metadata.status === "string" ? metadata.status : null;
  const fallbackStatus = fallbackProviderState?.conflicted === false
    && fallbackProviderState?.status === "idle"
    ? "idle"
    : null;
  const status = directStatus ?? fallbackStatus;
  if ((allowMissingStatus && status != null && status !== "idle") || (!allowMissingStatus && status !== "idle")) {
    throw new PermanentIncompleteError(
      "read_thread provider is not proven idle",
      "PROVIDER_CHANGED",
    );
  }
  return bridgeThread({
    id: threadId,
    kind: "chatgpt",
    title: metadata.title ?? "",
    createdAt: metadata.createdAt,
    updatedAt: metadata.updatedAt,
    status,
  }, Date.now() / 1000);
}

async function repairContinuation(
  client,
  threadId,
  contextThreadId,
  fallbackProviderState = null,
  directHistorical = false,
) {
  const baseline = atVerificationStageSync(
    "BASELINE",
    () => cliJson(["chatgpt-continuation-baseline", threadId]),
  );
  const providerBefore = directHistorical
    ? null
    : (fallbackProviderState?.thread ?? await atVerificationStage(
      "DISCOVERY_PRE",
      () => observeListThread(client, threadId, contextThreadId),
    ));
  const transcriptBefore = await atVerificationStage(
    "READ_PRE",
    () => observeReadThread(
      client,
      threadId,
      contextThreadId,
      fallbackProviderState,
      directHistorical,
    ),
  );
  const transcript = await atVerificationStage(
    "FULL_REPLAY",
    () => readCompleteThread(
      client,
      threadId,
      contextThreadId,
      transcriptBefore.update_time,
    ),
  );
  const transcriptAfter = await atVerificationStage(
    "READ_POST",
    () => observeReadThread(
      client,
      threadId,
      contextThreadId,
      fallbackProviderState,
      directHistorical,
    ),
  );
  if (transcriptBefore.update_time !== transcriptAfter.update_time) {
    throw new PermanentIncompleteError(
      "transcript provider changed across continuation replay",
      "PROVIDER_CHANGED",
    );
  }
  let providerAfter = null;
  if (!directHistorical) {
    providerAfter = await atVerificationStage(
      "DISCOVERY_POST",
      () => observeListThread(client, threadId, contextThreadId),
    );
    if (providerBefore.update_time !== providerAfter.update_time) {
      throw new PermanentIncompleteError(
        "discovery provider changed across continuation replay",
        "PROVIDER_CHANGED",
      );
    }
    atVerificationStageSync("OBSERVATION", () => recordProviderObservation(providerAfter));
  }
  const input = JSON.stringify({
    verification_scope: directHistorical ? "historical_transcript_direct" : "discovery_aligned",
    baseline,
    provider_before: providerBefore ?? transcriptBefore,
    provider_after: providerAfter ?? transcriptAfter,
    transcript_before: transcriptBefore,
    transcript_after: transcriptAfter,
    transcript,
  });
  atVerificationStageSync(
    "PUBLICATION",
    () => cliJson(
      ["chatgpt-repair-continuation", "--path", "-", "--stdin-bytes", String(Buffer.byteLength(input))],
      input,
    ),
  );
  return transcript;
}

function isRateLimit(error) {
  return error?.toolResultCategory === "rate_limited"
    || /too many requests|rate.?limit|429/iu.test(String(error?.message ?? error));
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

function writeJsonStatus(file, payload) {
  fs.mkdirSync(path.dirname(file), { recursive: true });
  const staged = `${file}.${process.pid}.tmp`;
  fs.writeFileSync(staged, `${JSON.stringify(payload, null, 2)}\n`, { mode: 0o600 });
  fs.renameSync(staged, file);
}

function readMemoryCompilerHistory(file = MEMORY_COMPILER_HISTORY_PATH) {
  try {
    const parsed = JSON.parse(fs.readFileSync(file, "utf8"));
    if (parsed == null || typeof parsed !== "object" || !Array.isArray(parsed.runs)) {
      throw new Error("invalid memory compiler history");
    }
    return {
      version: 1,
      total_runs: Number.isInteger(parsed.total_runs) ? parsed.total_runs : parsed.runs.length,
      last_success_at: typeof parsed.last_success_at === "string" ? parsed.last_success_at : null,
      last_failure_at: typeof parsed.last_failure_at === "string" ? parsed.last_failure_at : null,
      consecutive_failures: Number.isInteger(parsed.consecutive_failures)
        ? parsed.consecutive_failures
        : 0,
      runs: parsed.runs.slice(-MEMORY_COMPILER_HISTORY_LIMIT),
    };
  } catch {
    return {
      version: 1,
      total_runs: 0,
      last_success_at: null,
      last_failure_at: null,
      consecutive_failures: 0,
      runs: [],
    };
  }
}

function memoryCompilerRunSummary(payload) {
  return {
    state: payload.state ?? "unknown",
    started_at: payload.started_at ?? null,
    completed_at: payload.completed_at ?? payload.checked_at ?? null,
    model: payload.model ?? null,
    projects: Array.isArray(payload.results)
      ? payload.results.map((entry) => ({
        project: entry.project ?? null,
        status: entry.status ?? "unknown",
        ...(entry.status === "ok"
          ? {
            model_attempts: Number(entry.result?.model_attempts ?? 0),
            staged_candidates: Array.isArray(entry.result?.staged)
              ? entry.result.staged.reduce(
                (sum, staged) => sum + Number(staged?.candidate_ids?.length ?? 0),
                0,
              )
              : 0,
            failures: Array.isArray(entry.result?.failures) ? entry.result.failures.length : 0,
          }
          : entry.status === "blocked_by_health"
            ? {
              blockers: Array.isArray(entry.health?.blockers) ? entry.health.blockers.slice(0, 10) : [],
              warnings: Array.isArray(entry.health?.warnings) ? entry.health.warnings.slice(0, 10) : [],
            }
            : { error: String(entry.error ?? "").slice(0, 1_000) }),
      }))
      : [],
    ...(payload.error == null ? {} : { error: String(payload.error).slice(0, 1_000) }),
  };
}

function appendMemoryCompilerHistory(
  payload,
  file = MEMORY_COMPILER_HISTORY_PATH,
) {
  const history = readMemoryCompilerHistory(file);
  const failed = payload.state !== "completed";
  const eventAt = payload.completed_at ?? payload.checked_at ?? new Date().toISOString();
  const next = {
    version: 1,
    total_runs: history.total_runs + 1,
    last_success_at: failed ? history.last_success_at : eventAt,
    last_failure_at: failed ? eventAt : history.last_failure_at,
    consecutive_failures: failed ? history.consecutive_failures + 1 : 0,
    runs: [...history.runs, memoryCompilerRunSummary(payload)].slice(-MEMORY_COMPILER_HISTORY_LIMIT),
  };
  writeJsonStatus(file, next);
  return next;
}

function safeAppendMemoryCompilerHistory(
  payload,
  file = MEMORY_COMPILER_HISTORY_PATH,
  onError = (error) => appendLog(ERROR_LOG_PATH, {
    event: "memory_auto_compile_history_error",
    error: String(error?.message ?? error),
  }),
) {
  try {
    return appendMemoryCompilerHistory(payload, file);
  } catch (error) {
    onError(error);
    return null;
  }
}

function memoryCompilerHistorySummary(history) {
  return {
    total_runs: history.total_runs,
    last_success_at: history.last_success_at,
    last_failure_at: history.last_failure_at,
    consecutive_failures: history.consecutive_failures,
    recent_runs: history.runs.slice(-MEMORY_COMPILER_STATUS_RECENT_RUNS),
  };
}

function boundedInteger(value, fallback, min, max, name) {
  if (value == null || String(value).trim() === "") return fallback;
  const parsed = Number(value);
  if (!Number.isInteger(parsed) || parsed < min || parsed > max) {
    throw new Error(`${name} must be an integer between ${min} and ${max}`);
  }
  return parsed;
}

function memoryAutoConfig(env = process.env) {
  const model = String(env.CHAT_HISTORY_MEMORY_MODEL ?? "").trim() || null;
  const projects = [...new Set(
    String(env.CHAT_HISTORY_MEMORY_AUTO_PROJECTS ?? "")
      .split(",")
      .map((project) => project.trim())
      .filter(Boolean),
  )];
  if (projects.length === 0) {
    return {
      enabled: false,
      projects: [],
      scanLimit: DEFAULT_MEMORY_AUTO_SCAN_LIMIT,
      maxConversations: DEFAULT_MEMORY_AUTO_MAX_CONVERSATIONS,
      maxMessages: DEFAULT_MEMORY_AUTO_MAX_MESSAGES,
      maxPendingCandidates: DEFAULT_MEMORY_AUTO_MAX_PENDING_CANDIDATES,
      model,
    };
  }
  if (projects.length > 10) {
    throw new Error("CHAT_HISTORY_MEMORY_AUTO_PROJECTS supports at most 10 projects");
  }
  if (model == null) {
    throw new Error(
      "CHAT_HISTORY_MEMORY_MODEL is required when CHAT_HISTORY_MEMORY_AUTO_PROJECTS is enabled",
    );
  }
  return {
    enabled: true,
    projects,
    scanLimit: boundedInteger(
      env.CHAT_HISTORY_MEMORY_AUTO_SCAN_LIMIT,
      DEFAULT_MEMORY_AUTO_SCAN_LIMIT,
      1,
      1_000,
      "CHAT_HISTORY_MEMORY_AUTO_SCAN_LIMIT",
    ),
    maxConversations: boundedInteger(
      env.CHAT_HISTORY_MEMORY_AUTO_MAX_CONVERSATIONS,
      DEFAULT_MEMORY_AUTO_MAX_CONVERSATIONS,
      1,
      10,
      "CHAT_HISTORY_MEMORY_AUTO_MAX_CONVERSATIONS",
    ),
    maxMessages: boundedInteger(
      env.CHAT_HISTORY_MEMORY_AUTO_MAX_MESSAGES,
      DEFAULT_MEMORY_AUTO_MAX_MESSAGES,
      1,
      16,
      "CHAT_HISTORY_MEMORY_AUTO_MAX_MESSAGES",
    ),
    maxPendingCandidates: boundedInteger(
      env.CHAT_HISTORY_MEMORY_AUTO_MAX_PENDING_CANDIDATES,
      DEFAULT_MEMORY_AUTO_MAX_PENDING_CANDIDATES,
      MAX_MEMORY_CANDIDATES_PER_MODEL_ATTEMPT,
      100,
      "CHAT_HISTORY_MEMORY_AUTO_MAX_PENDING_CANDIDATES",
    ),
    model,
  };
}

function memoryAutoHealthGate(health, maxPendingCandidates, maxConversations = 1) {
  const blockers = [];
  const warnings = [];
  const requiredCount = (value, name) => {
    const parsed = Number(value);
    if (!Number.isInteger(parsed) || parsed < 0) {
      throw new Error(`invalid memory health count: ${name}`);
    }
    return parsed;
  };
  const warningCount = (value) => {
    if (value == null) return 0;
    const parsed = Number(value);
    return Number.isInteger(parsed) && parsed >= 0 ? parsed : 0;
  };
  const checkpointPrefixProblems = requiredCount(
    health?.checkpoint_prefix_problem,
    "checkpoint_prefix_problem",
  );
  const pendingRevalidationProblems = requiredCount(
    health?.pending_revalidation_problem_count,
    "pending_revalidation_problem_count",
  );
  const pendingCandidates = requiredCount(
    health?.candidates?.pending,
    "candidates.pending",
  );
  const modelAttempts = requiredCount(maxConversations, "max_conversations");
  if (modelAttempts < 1 || modelAttempts > 10) {
    throw new Error("invalid memory health count: max_conversations");
  }
  const maxNewCandidates = modelAttempts * MAX_MEMORY_CANDIDATES_PER_MODEL_ATTEMPT;
  const incompleteCanonical = warningCount(health?.incomplete_canonical_conversation_count);
  const staleOrUnverified = warningCount(health?.active_stale_or_unverified);
  const rejectedSnapshots = warningCount(health?.tracked_rejected_lower_quality_snapshots);

  if (checkpointPrefixProblems > 0) {
    blockers.push({
      code: "checkpoint_prefix_problem",
      count: checkpointPrefixProblems,
    });
  }
  if (pendingRevalidationProblems > 0) {
    blockers.push({
      code: "pending_revalidation_problem",
      count: pendingRevalidationProblems,
    });
  }
  if (pendingCandidates + maxNewCandidates > maxPendingCandidates) {
    blockers.push({
      code: "pending_candidate_backlog",
      count: pendingCandidates,
      limit: maxPendingCandidates,
      max_new_candidates: maxNewCandidates,
      required_headroom: maxNewCandidates,
    });
  }
  if (incompleteCanonical > 0) {
    warnings.push({
      code: "incomplete_canonical_evidence",
      count: incompleteCanonical,
    });
  }
  if (staleOrUnverified > 0) {
    warnings.push({
      code: "stale_or_unverified_active_memory",
      count: staleOrUnverified,
    });
  }
  if (rejectedSnapshots > 0) {
    warnings.push({
      code: "rejected_lower_quality_evidence",
      count: rejectedSnapshots,
    });
  }
  return {
    ok: blockers.length === 0,
    blockers,
    warnings,
    summary: {
      checkpoint_prefix_problem: checkpointPrefixProblems,
      pending_revalidation_problem_count: pendingRevalidationProblems,
      pending_candidates: pendingCandidates,
      max_pending_candidates: maxPendingCandidates,
      incomplete_canonical_conversation_count: incompleteCanonical,
      active_stale_or_unverified: staleOrUnverified,
      tracked_rejected_lower_quality_snapshots: rejectedSnapshots,
    },
  };
}

function memoryAutoTriggerConversationIds(env = process.env) {
  const raw = String(env[MEMORY_AUTO_TRIGGER_ENV] ?? "").trim();
  if (!raw) return [];
  let parsed;
  try {
    parsed = JSON.parse(raw);
  } catch {
    throw new Error(MEMORY_AUTO_TRIGGER_ENV + " must be a JSON array");
  }
  if (!Array.isArray(parsed)) {
    throw new Error(MEMORY_AUTO_TRIGGER_ENV + " must be a JSON array");
  }
  const ids = [...new Set(parsed.map((value) => String(value).trim()).filter(Boolean))];
  if (ids.length > DISCOVERY_LIMIT) {
    throw new Error(MEMORY_AUTO_TRIGGER_ENV + " supports at most " + DISCOVERY_LIMIT + " conversations");
  }
  return ids;
}

function maybeScheduleMemoryCompiler(syncResult, spawnImpl = spawn, env = process.env) {
  const config = memoryAutoConfig(env);
  if (!config.enabled || !(Number(syncResult?.imported) > 0)) {
    return { enabled: config.enabled, scheduled: false };
  }
  const conversationIds = [...new Set(
    (Array.isArray(syncResult?.conversation_ids) ? syncResult.conversation_ids : [])
      .map((value) => String(value).trim())
      .filter(Boolean),
  )].slice(0, DISCOVERY_LIMIT);
  if (conversationIds.length === 0) {
    return {
      enabled: true,
      scheduled: false,
      reason: "no_imported_conversation_ids",
    };
  }
  const child = spawnImpl(process.execPath, [process.argv[1], "--memory-compiler-worker"], {
    detached: true,
    stdio: "ignore",
    env: {
      ...env,
      [MEMORY_AUTO_TRIGGER_ENV]: JSON.stringify(conversationIds),
    },
  });
  child.unref?.();
  return {
    enabled: true,
    scheduled: true,
    pid: child.pid ?? null,
    projects: config.projects,
    model: config.model,
    max_conversations: config.maxConversations,
    max_messages: config.maxMessages,
    conversation_ids: conversationIds,
  };
}

function safeScheduleMemoryCompiler(
  syncResult,
  spawnImpl = spawn,
  env = process.env,
  onError = (payload) => appendLog(ERROR_LOG_PATH, payload),
) {
  try {
    return maybeScheduleMemoryCompiler(syncResult, spawnImpl, env);
  } catch (error) {
    const detail = String(error?.message ?? error);
    onError({
      event: "memory_auto_compile_schedule_error",
      error: detail,
    });
    return {
      enabled: String(env.CHAT_HISTORY_MEMORY_AUTO_PROJECTS ?? "").trim().length > 0,
      scheduled: false,
      error: detail,
    };
  }
}

function runMemoryCompilerWorker() {
  mustExist(CLI);
  const config = memoryAutoConfig();
  if (!config.enabled) return { event: "memory_auto_compile_skipped", reason: "disabled" };
  const triggerConversationIds = memoryAutoTriggerConversationIds();
  if (triggerConversationIds.length === 0) {
    return { event: "memory_auto_compile_skipped", reason: "no_trigger_conversations" };
  }
  const releaseLock = acquirePidLock(MEMORY_COMPILER_LOCK);
  if (releaseLock == null) return { event: "memory_auto_compile_skipped", reason: "locked" };
  const startedAt = new Date().toISOString();
  writeJsonStatus(MEMORY_COMPILER_STATUS_PATH, {
    state: "running",
    pid: process.pid,
    started_at: startedAt,
    projects: config.projects,
    model: config.model,
    scan_limit: config.scanLimit,
    max_conversations: config.maxConversations,
    max_messages: config.maxMessages,
    max_pending_candidates: config.maxPendingCandidates,
    trigger_conversation_ids: triggerConversationIds,
  });
  const results = [];
  let failed = false;
  try {
    for (const project of config.projects) {
      try {
        const health = cliJson([
          "memory-health",
          "--project", project,
        ]);
        const healthGate = memoryAutoHealthGate(
          health,
          config.maxPendingCandidates,
          config.maxConversations,
        );
        if (!healthGate.ok) {
          results.push({
            project,
            status: "blocked_by_health",
            health: healthGate,
          });
          continue;
        }
        const result = {
          project,
          triggered: triggerConversationIds.length,
          matched: 0,
          caught_up: 0,
          model_attempts: 0,
          staged: [],
          failures: [],
        };
        for (const conversationId of triggerConversationIds) {
          const match = cliJson([
            "memory-project-match",
            "--project", project,
            conversationId,
          ]);
          if (match?.strong_match !== true) continue;
          result.matched += 1;
          if (result.model_attempts >= config.maxConversations) continue;
          result.model_attempts += 1;
          try {
            const compiled = cliJson([
              "memory-compile-conversation",
              "--project", project,
              "--max-messages", String(config.maxMessages),
              conversationId,
            ]);
            if (compiled?.status === "caught_up" || compiled?.result == null) {
              result.caught_up += 1;
              continue;
            }
            const run = compiled.result;
            result.staged.push({
              conversation_id: conversationId,
              source_snapshot_id: run?.input?.source_snapshot_id ?? null,
              through_turn_index: run?.input?.through_turn_index ?? null,
              candidate_ids: Array.isArray(run?.staged?.candidate_ids)
                ? run.staged.candidate_ids
                : [],
            });
          } catch (error) {
            result.failures.push({
              conversation_id: conversationId,
              error: String(error?.message ?? error),
            });
          }
        }
        results.push({ project, status: "ok", result });
      } catch (error) {
        failed = true;
        results.push({
          project,
          status: "error",
          error: String(error?.message ?? error),
        });
      }
    }
    const payload = {
      event: "memory_auto_compile",
      state: failed ? "degraded" : "completed",
      pid: process.pid,
      started_at: startedAt,
      completed_at: new Date().toISOString(),
      projects: config.projects,
      model: config.model,
      trigger_conversation_ids: triggerConversationIds,
      results,
    };
    writeJsonStatus(MEMORY_COMPILER_STATUS_PATH, payload);
    safeAppendMemoryCompilerHistory(payload);
    appendLog(failed ? ERROR_LOG_PATH : LOG_PATH, payload);
    return payload;
  } finally {
    releaseLock();
  }
}

function acquirePidLock(lock) {
  fs.mkdirSync(path.dirname(lock), { recursive: true });
  for (let attempt = 0; attempt < 2; attempt += 1) {
    try {
      fs.mkdirSync(lock);
      fs.writeFileSync(path.join(lock, "pid"), `${process.pid}\n`, { mode: 0o600 });
      return () => {
        let ownerPid = null;
        try {
          ownerPid = Number(fs.readFileSync(path.join(lock, "pid"), "utf8").trim());
        } catch {}
        if (ownerPid === process.pid) {
          fs.rmSync(lock, { recursive: true, force: true });
        }
      };
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
    const previousRateLimited = priorRateLimitedThread();
    const restore = historicalRestoreCandidate(
      pendingHistoricalRestores(16),
      previousRateLimited,
    );
    if (restore != null) {
      try {
        const transcript = await repairContinuation(
          client,
          restore.thread_id,
          contextThreadId,
          null,
          true,
        );
        return {
          event: "chatgpt_live_sync",
          imported: 1,
          blocked: 0,
          deferred_active: 0,
          titles: [transcript.title],
          conversation_ids: [restore.thread_id],
        };
      } catch (error) {
        if (isRateLimit(error)) {
          return {
            event: "chatgpt_live_rate_limited",
            imported: 0,
            pending: restore.thread_id,
            conversation_ids: [],
          };
        }
        const stage = typeof error?.verificationStage === "string"
          && new Set(["BASELINE", "READ_PRE", "FULL_REPLAY", "READ_POST", "PUBLICATION"])
            .has(error.verificationStage)
          ? error.verificationStage
          : "UNKNOWN";
        if (/App Tools request timed out:/u.test(String(error?.message ?? ""))) {
          appendLog(ERROR_LOG_PATH, {
            event: "chatgpt_live_restore_verification_deferred",
            thread_id: restore.thread_id.slice(0, 256),
            code: repairFailureCode(error),
            stage,
            reason: "Historical restore verification hit a transient App Tools timeout",
          });
          return {
            event: "chatgpt_live_restore_verification_deferred",
            imported: 0,
            blocked: 0,
            deferred_active: 1,
            pending: restore.thread_id,
            conversation_ids: [],
          };
        }
        appendLog(ERROR_LOG_PATH, {
          event: "chatgpt_live_restore_verification_rejected",
          thread_id: restore.thread_id.slice(0, 256),
          code: repairFailureCode(error),
          error_class: verificationErrorClass(error),
          detail_class: verificationDetailClass(error),
          stage,
          reason: "Historical restore direct transcript verification was not proven",
        });
        return {
          event: "chatgpt_live_sync",
          imported: 0,
          blocked: 1,
          deferred_active: 0,
          titles: [],
          conversation_ids: [],
        };
      }
    }

    const catalog = toolText(await client.callTool("list_threads", { limit: DISCOVERY_LIMIT }, contextThreadId));
    const observedAt = Date.now() / 1000;
    const providerStateById = new Map();
    for (const entry of [...(catalog.threads ?? []), ...(catalog.pinnedThreads ?? [])]) {
      const state = {
        status: typeof entry.status === "string" ? entry.status : null,
        revision: Number.isFinite(entry.updatedAt) ? entry.updatedAt : null,
        conflicted: false,
        thread: bridgeThread(entry, observedAt),
      };
      const existing = providerStateById.get(entry.id);
      if (existing == null) providerStateById.set(entry.id, state);
      else if (existing.status !== state.status || existing.revision !== state.revision) {
        existing.conflicted = true;
      }
    }
    const snapshot = {
      requested_limit: DISCOVERY_LIMIT,
      threads: (catalog.threads ?? []).map((entry) => bridgeThread(entry, observedAt)),
      pinned_threads: (catalog.pinnedThreads ?? []).map((entry) => bridgeThread(entry, observedAt)),
    };
    const snapshotText = JSON.stringify(snapshot);
    const planned = cliJson([
      "chatgpt-plan-recent",
      "--path", "-",
      "--stdin-bytes", String(Buffer.byteLength(snapshotText)),
    ], snapshotText);
    const ordinarySelected = planned.plan?.selected ?? [];
    const repairIds = planned.plan?.skipped_blocked_ids ?? [];
    const selected = buildSelectedQueue(
      ordinarySelected,
      repairIds,
      previousRateLimited,
    );
    if (selected.length === 0) {
      return {
        event: "chatgpt_live_sync",
        imported: 0,
        blocked: 0,
        deferred_active: 0,
        titles: [],
        conversation_ids: [],
      };
    }

    let imported = 0;
    let deferredActive = 0;
    let blocked = 0;
    const importedTitles = [];
    const importedConversationIds = [];
    for (const pending of selected) {
      const providerState = providerStateById.get(pending.thread_id);
      if (providerState == null || providerState.conflicted || providerState.status !== "idle") {
        deferredActive += 1;
        continue;
      }
      try {
        const transcript = pending.continuation_repair
          ? await repairContinuation(
            client,
            pending.thread_id,
            contextThreadId,
            providerState,
            false,
          )
          : await readCompleteThread(client, pending.thread_id, contextThreadId);
        if (!pending.continuation_repair) importTranscript(transcript);
        imported += 1;
        importedTitles.push(transcript.title);
        importedConversationIds.push(pending.thread_id);
      } catch (error) {
        if (pending.continuation_repair) {
          if (isRateLimit(error)) return {
            event: "chatgpt_live_rate_limited", imported, pending: pending.thread_id,
            conversation_ids: importedConversationIds,
          };
          const diagnostic = recordRepairFailure(pending.thread_id, error);
          appendLog(ERROR_LOG_PATH, {
            event: "chatgpt_live_repair_rejected", thread_id: pending.thread_id.slice(0, 256),
            ...diagnostic,
          });
          blocked += 1;
          continue;
        }
        if (isRateLimit(error)) {
          return {
            event: "chatgpt_live_rate_limited",
            imported,
            pending: pending.thread_id,
            conversation_ids: importedConversationIds,
          };
        }
        if (error instanceof PermanentIncompleteError) {
          if (!pending.continuation_repair) markBlocked(pending.thread_id, error.message);
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
      conversation_ids: importedConversationIds,
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
      const memoryScheduler = safeScheduleMemoryCompiler(result);
      const status = {
        state: "running",
        pid: process.pid,
        checked_at: new Date().toISOString(),
        pipe_present: true,
        pipe_connected: !client.closed,
        pipe_identity: currentPipeIdentity,
        interval_ms: interval,
        last_result: result,
        memory_scheduler: memoryScheduler,
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
  let pid = null;
  try {
    pid = Number(fs.readFileSync(path.join(DAEMON_LOCK, "pid"), "utf8").trim());
  } catch {}

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
    if (pidAlive(pid)) {
      throw new Error(`existing collector daemon ${pid} did not stop cleanly`);
    }
  }
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
      try {
        status = {
          ...status,
          memory_compiler: JSON.parse(
            fs.readFileSync(MEMORY_COMPILER_STATUS_PATH, "utf8"),
          ),
        };
      } catch {}
      try {
        status = {
          ...status,
          memory_compiler_history: memoryCompilerHistorySummary(
            readMemoryCompilerHistory(),
          ),
        };
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
  if (process.argv.includes("--memory-compiler-worker")) {
    try {
      runMemoryCompilerWorker();
    } catch (error) {
      const detail = String(error?.stack ?? error);
      writeJsonStatus(MEMORY_COMPILER_STATUS_PATH, {
        state: "degraded",
        pid: process.pid,
        checked_at: new Date().toISOString(),
        error: detail,
      });
      safeAppendMemoryCompilerHistory({
        state: "degraded",
        pid: process.pid,
        checked_at: new Date().toISOString(),
        error: detail,
      });
      appendLog(ERROR_LOG_PATH, { event: "memory_auto_compile_worker_error", error: detail });
      process.exitCode = 1;
    }
  } else if (process.argv.includes("--daemon")) {
    daemonLoop().catch((error) => {
      appendLog(ERROR_LOG_PATH, { event: "chatgpt_live_collector_fatal", error: String(error?.stack ?? error) });
      process.exitCode = 1;
    });
  } else if (process.argv.includes("--mcp-sidecar")) {
    runMcpSidecar();
  } else {
    syncOnce()
      .then((result) => {
        const memoryScheduler = safeScheduleMemoryCompiler(result);
        if (memoryScheduler.enabled) result.memory_scheduler = memoryScheduler;
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
  acquirePidLock,
  bridgeMessages,
  bridgeThread,
  buildSelectedQueue,
  historicalRestoreCandidate,
  ensureDaemonReady,
  memoryAutoConfig,
  memoryAutoHealthGate,
  memoryAutoTriggerConversationIds,
  appendMemoryCompilerHistory,
  memoryCompilerHistorySummary,
  messageText,
  maybeScheduleMemoryCompiler,
  safeScheduleMemoryCompiler,
  readMemoryCompilerHistory,
  readCompleteThread,
  repairContinuation,
  verificationErrorClass,
  verificationDetailClass,
  runMemoryCompilerWorker,
  syncWithClient,
  syncOnce,
};
