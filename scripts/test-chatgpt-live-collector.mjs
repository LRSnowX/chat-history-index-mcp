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
  memoryAutoHealthGate,
  memoryAutoTriggerConversationIds,
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
      status: null,
      observed_at: null,
    },
  );
  assert.equal(bridgeThread({ id: "thread-1", kind: "chatgpt" }, 30).observed_at, 30);
  assert.throws(
    () => memoryAutoConfig({
      CHAT_HISTORY_MEMORY_AUTO_PROJECTS: "LEMonX",
    }),
    /CHAT_HISTORY_MEMORY_MODEL is required/,
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

test("collector records statuses and only imports idle threads; incomplete idle stays blocked", () => {
  const root = fs.mkdtempSync(path.join(os.tmpdir(), "chim-source-integrity-"));
  try {
    fs.mkdirSync(path.join(root, "bin"), { recursive: true });
    const callsPath = path.join(root, "calls.jsonl");
    const cli = `#!${process.execPath}
import fs from 'node:fs';
const input = fs.readFileSync(0, 'utf8');
fs.appendFileSync(${JSON.stringify(callsPath)}, JSON.stringify({args:process.argv.slice(2),input:input ? JSON.parse(input) : null})+'\\n');
if (process.argv[2] === 'chatgpt-plan-recent') console.log(JSON.stringify({plan:{selected:JSON.parse(input).threads.map(t=>({...t}))}}));
else console.log('{}');
`;
    // .mjs target plus a shell-free executable wrapper at the normal CLI path.
    fs.writeFileSync(path.join(root, "bin", "package.json"), '{"type":"module"}');
    fs.writeFileSync(path.join(root, "bin", "chat-history-cli"), cli, { mode: 0o755 });
    const collector = new URL("./chatgpt-live-collector.mjs", import.meta.url).href;
    const child = spawnSync(process.execPath, ["--input-type=module", "-e", `
      import { syncWithClient } from ${JSON.stringify(collector)};
      const reads = [];
      const client = { async callTool(name, args) {
        if (name === 'list_threads') return {content:[{type:'text',text:JSON.stringify({threads:
          ['idle','active','unknown',null].map((status,index)=>({id:'thread-'+index,kind:'chatgpt',title:'Arcos',createdAt:10,updatedAt:20,status}))
        })}]};
        reads.push(args.threadId);
        return {content:[{type:'text',text:JSON.stringify({thread:{title:'Arcos',createdAt:10,updatedAt:20},
          turns:[{id:'turn',items:[{type:'agentMessage',id:'large',text:'x'.repeat(19990)}]}],page:{hasMore:false,order:'newest_first'}})}]};
      }};
      const result = await syncWithClient(client, 'context');
      console.log(JSON.stringify({result,reads}));
    `], { encoding: "utf8", env: { ...process.env, CHAT_HISTORY_DATA_HOME: root } });
    assert.equal(child.status, 0, child.stderr);
    const output = JSON.parse(child.stdout);
    assert.deepEqual(output.reads, ["thread-0"]);
    assert.equal(output.result.deferred_active, 3);
    assert.equal(output.result.blocked, 1);
    assert.equal(output.result.imported, 0);
    const calls = fs.readFileSync(callsPath, "utf8").trim().split("\n").map(JSON.parse);
    assert.deepEqual(calls[0].input.threads.map((thread) => thread.status), ["idle", "active", "unknown", null]);
    assert.ok(calls[0].input.threads.every((thread) => Number.isFinite(thread.observed_at)));
    assert.equal(calls[1].args[0], "chatgpt-block");
    assert.match(calls[1].args.at(-1), /safety limit/);
    assert.equal(calls.some((call) => call.args[0] === "chatgpt-import-thread"), false);
  } finally {
    fs.rmSync(root, { recursive: true, force: true });
  }
});

test("collector fails closed when recent and pinned metadata conflict for one thread", () => {
  const root = fs.mkdtempSync(path.join(os.tmpdir(), "chim-source-integrity-conflict-"));
  try {
    fs.mkdirSync(path.join(root, "bin"), { recursive: true });
    const callsPath = path.join(root, "calls.jsonl");
    const cli = `#!${process.execPath}\nimport fs from 'node:fs';\nconst input = fs.readFileSync(0, 'utf8');\nfs.appendFileSync(${JSON.stringify(callsPath)}, JSON.stringify({args:process.argv.slice(2),input:JSON.parse(input)})+'\\n');\nconsole.log(JSON.stringify({plan:{selected:[JSON.parse(input).threads[0]]}}));\n`;
    fs.writeFileSync(path.join(root, "bin", "package.json"), '{"type":"module"}');
    fs.writeFileSync(path.join(root, "bin", "chat-history-cli"), cli, { mode: 0o755 });
    const collector = new URL("./chatgpt-live-collector.mjs", import.meta.url).href;
    const child = spawnSync(process.execPath, ["--input-type=module", "-e", `
      import { syncWithClient } from ${JSON.stringify(collector)};
      const reads = [];
      const client = { async callTool(name, args) {
        if (name === 'list_threads') return {content:[{type:'text',text:JSON.stringify({
          threads:[{id:'same',kind:'chatgpt',title:'Arcos',createdAt:10,updatedAt:20,status:'idle'}],
          pinnedThreads:[{id:'same',kind:'chatgpt',title:'Arcos',createdAt:10,updatedAt:21,status:'active'}]
        })}]};
        reads.push(args.threadId);
        throw new Error('conflicted thread must not be read');
      }};
      const result = await syncWithClient(client, 'context');
      console.log(JSON.stringify({result,reads}));
    `], { encoding: "utf8", env: { ...process.env, CHAT_HISTORY_DATA_HOME: root } });
    assert.equal(child.status, 0, child.stderr);
    const output = JSON.parse(child.stdout);
    assert.deepEqual(output.reads, []);
    assert.equal(output.result.imported, 0);
    assert.equal(output.result.deferred_active, 1);
    const call = JSON.parse(fs.readFileSync(callsPath, "utf8").trim());
    assert.equal(call.input.threads[0].observed_at, call.input.pinned_threads[0].observed_at);
    assert.ok(Number.isFinite(call.input.threads[0].observed_at));
  } finally {
    fs.rmSync(root, { recursive: true, force: true });
  }
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

test("continuation collector uses an auditable repair path with pre/post idle observations", () => {
  const root = fs.mkdtempSync(path.join(os.tmpdir(), "chim-continuation-"));
  try {
    fs.mkdirSync(path.join(root, "bin"), { recursive: true });
    const callsPath = path.join(root, "calls.jsonl");
    fs.writeFileSync(path.join(root, "bin", "package.json"), '{"type":"module"}');
    fs.writeFileSync(path.join(root, "bin", "chat-history-cli"), `#!${process.execPath}
import fs from 'node:fs';
const input = fs.readFileSync(0, 'utf8');
fs.appendFileSync(${JSON.stringify(callsPath)}, JSON.stringify({args:process.argv.slice(2),input:input ? JSON.parse(input) : null})+'\\n');
console.log(JSON.stringify(process.argv[2] === 'chatgpt-continuation-baseline' ? {conversation_id:'thread',baseline_token:'test-baseline'} : {plan:{selected:[],skipped_blocked_ids:['thread']}}));
`, { mode: 0o755 });
    const collector = new URL("./chatgpt-live-collector.mjs", import.meta.url).href;
    for (const scenario of ["success", "sync", "revision", "active", "unknown", "missing", "page_changed", "contradictory"]) {
      fs.writeFileSync(callsPath, "");
      const child = spawnSync(process.execPath, ["--input-type=module", "-e", `
        import { repairContinuation, syncWithClient } from ${JSON.stringify(collector)};
        const scenario = ${JSON.stringify(scenario)};
        let observations = 0;
        const reads = [];
        const client = { async callTool(name,args) {
          let payload;
          if (name === 'list_threads') {
            observations++;
            const entry = {id:'thread',kind:'chatgpt',title:'synthetic',updatedAt:20,status:'idle'};
            if (observations === 2) {
              if (scenario === 'revision') entry.updatedAt=21;
              if (scenario === 'active' || scenario === 'unknown') entry.status=scenario;
            }
            payload={threads:observations === 2 && scenario === 'missing' ? [] : [entry]};
            if (observations === 2 && scenario === 'contradictory') payload.pinnedThreads=[{...entry,updatedAt:21}];
          } else {
            reads.push(args);
            payload={thread:{id:'thread',title:'synthetic',updatedAt:scenario === 'page_changed' ? 21 : 20},
              page:{order:'newest_first',hasMore:false},turns:[{id:'turn',status:'completed',items:[
                {type:'userMessage',id:'old',content:[{type:'text',text:'x'.repeat(19990)}]},
                {type:'agentMessage',id:'new',text:'complete new tail'}]}]};
          }
          return {content:[{type:'text',text:JSON.stringify(payload)}]};
        }};
        try {
          if (scenario === 'sync') {
            const result=await syncWithClient(client,'context');
            if (result.imported !== 1) throw new Error('blocked thread did not reach repair');
          } else await repairContinuation(client,'thread','context');
          console.log(JSON.stringify({ok:true,reads}));
        }
        catch(error) { console.log(JSON.stringify({ok:false,message:error.message,reads})); }
      `], { encoding: "utf8", env: { ...process.env, CHAT_HISTORY_DATA_HOME: root } });
      assert.equal(child.status, 0, child.stderr);
      const output = JSON.parse(child.stdout);
      const successful = scenario === "success" || scenario === "sync";
      assert.equal(output.ok, successful, scenario);
      assert.equal(output.reads[0].maxOutputCharsPerItem, 20_000);
      assert.equal(output.reads[0].includeOutputs, true);
      const calls = fs.readFileSync(callsPath, "utf8").trim().split("\n").map(JSON.parse);
      assert.equal(calls[0].args[0], scenario === "sync" ? "chatgpt-plan-recent" : "chatgpt-continuation-baseline");
      const repair = calls.find((call) => call.args[0] === "chatgpt-repair-continuation");
      assert.equal(Boolean(repair), successful, scenario);
      assert.equal(calls.some((call) => call.args[0] === "chatgpt-import-thread" || call.args[0] === "chatgpt-unblock"), false);
      if (repair) {
        assert.equal(repair.input.provider_before.status, "idle");
        assert.equal(repair.input.provider_after.update_time, 20);
        assert.ok(repair.input.provider_after.observed_at >= repair.input.provider_before.observed_at);
        assert.equal(repair.input.transcript.pages[0].messages[1].truncated, true);
        assert.ok(repair.input.transcript.pages[0].messages.every((message) => message.stable_identity));
      }
      if (scenario !== "page_changed") assert.equal(calls.filter((call) => call.args[0] === "chatgpt-plan-recent").length, scenario === "sync" ? 3 : 2);
    }
  } finally {
    fs.rmSync(root, { recursive: true, force: true });
  }
});

test("synthetic fallback item identities cannot prove continuation", () => {
  const messages = bridgeMessages({ id: "turn", status: "completed", items: [{ type: "agentMessage", text: "no source ID" }] });
  assert.equal(messages[0].stable_identity, false);
});

test("live repair persists bounded codes and never logs raw CLI/provider message bodies", () => {
  const root = fs.mkdtempSync(path.join(os.tmpdir(), "chim-repair-diagnostics-"));
  try {
    fs.mkdirSync(path.join(root, "bin"), { recursive: true });
    const callsPath = path.join(root, "calls.jsonl");
    fs.writeFileSync(path.join(root, "bin", "package.json"), '{"type":"module"}');
    fs.writeFileSync(path.join(root, "bin", "chat-history-cli"), `#!${process.execPath}
import fs from 'node:fs';
const input=fs.readFileSync(0,'utf8');
const args=process.argv.slice(2);
fs.appendFileSync(${JSON.stringify(callsPath)},JSON.stringify(args)+'\\n');
if(args[0]==='chatgpt-repair-continuation') {
  console.error('CHIM_REPAIR_'+process.env.TEST_REPAIR_CODE+': '+ 'PRIVATE_BODY_SENTINEL'.repeat(2000));
  process.exit(1);
}
console.log(JSON.stringify(args[0]==='chatgpt-continuation-baseline' ? {} : args[0]==='chatgpt-record-repair-failure' ? {recorded:true,diagnostic:{code:args.at(-1),reason:'PRIVATE_BODY_SENTINEL'}} : {plan:{selected:[],skipped_blocked_ids:['thread']}}));
`, { mode: 0o755 });
    const collector = new URL("./chatgpt-live-collector.mjs", import.meta.url).href;
    for (const code of ["TRUNCATED_NEW_TAIL", "MISSING_OVERLAP", "PROVIDER_CHANGED", "REPLAY_INCOMPLETE", "BASELINE_CHANGED", "UNKNOWN_PROVIDER_FAILURE"]) {
      const child = spawnSync(process.execPath, ["--input-type=module", "-e", `
        import { syncWithClient } from ${JSON.stringify(collector)};
        const client={async callTool(name,args) {
          if(name==='read_thread' && process.env.TEST_REPAIR_CODE==='UNKNOWN_PROVIDER_FAILURE') throw new Error('PRIVATE_BODY_SENTINEL'.repeat(2000));
          const payload=name==='list_threads' ? {threads:[{id:'thread',title:'synthetic',kind:'chatgpt',status:'idle',updatedAt:30}]} :
            {thread:{id:'thread',title:'synthetic',updatedAt:30},page:{order:'newest_first',hasMore:false},turns:[{id:'turn',status:'completed',items:[{id:'new',type:'agentMessage',text:'complete synthetic body'}]}]};
          return {content:[{type:'text',text:JSON.stringify(payload)}]};
        }};
        console.log(JSON.stringify(await syncWithClient(client,'context')));
      `], { encoding: "utf8", env: { ...process.env, CHAT_HISTORY_DATA_HOME: root, TEST_REPAIR_CODE: code } });
      assert.equal(child.status, 0, child.stderr);
      assert.equal(JSON.parse(child.stdout).imported, 0);
      assert.equal(JSON.parse(child.stdout).blocked, 1);
      assert.equal(child.stdout.includes("PRIVATE_BODY_SENTINEL"), false);
      assert.equal(child.stderr.includes("PRIVATE_BODY_SENTINEL"), false);
      const records = fs.readFileSync(path.join(root,"logs/chatgpt-live-collector.error.log"),"utf8").trim().split("\n").map(JSON.parse);
      const diagnostic = records.at(-1);
      assert.equal(diagnostic.code, code === "UNKNOWN_PROVIDER_FAILURE" ? "REPLAY_INCOMPLETE" : code);
      assert.equal(diagnostic.diagnostic_persisted, true);
      assert.ok(diagnostic.reason.length <= 240);
      assert.equal(JSON.stringify(diagnostic).includes("PRIVATE_BODY_SENTINEL"), false);
      const calls = fs.readFileSync(callsPath,"utf8").trim().split("\n").map(JSON.parse);
      assert.equal(calls.some((args) => args[0] === "chatgpt-block" || args[0] === "chatgpt-import-thread"),false,"failed repair must not replace the original blocker or canonical history");
      assert.deepEqual(calls.at(-1),["chatgpt-record-repair-failure","thread","--code",diagnostic.code]);
    }
  } finally {
    fs.rmSync(root,{recursive:true,force:true});
  }
});

test("continuation repair rate limits stay transient and do not persist repair diagnostics", () => {
  const root = fs.mkdtempSync(path.join(os.tmpdir(), "chim-repair-rate-limit-"));
  try {
    fs.mkdirSync(path.join(root, "bin"), { recursive: true });
    const callsPath = path.join(root, "calls.jsonl");
    fs.writeFileSync(path.join(root, "bin", "package.json"), '{"type":"module"}');
    fs.writeFileSync(path.join(root, "bin", "chat-history-cli"), `#!${process.execPath}
import fs from 'node:fs';
const args=process.argv.slice(2);
fs.appendFileSync(${JSON.stringify(callsPath)},JSON.stringify(args)+'\\n');
console.log(JSON.stringify(args[0]==='chatgpt-continuation-baseline' ? {} : {plan:{selected:[],skipped_blocked_ids:['thread']}}));
`, { mode: 0o755 });
    const collector = new URL("./chatgpt-live-collector.mjs", import.meta.url).href;
    const child = spawnSync(process.execPath, ["--input-type=module", "-e", `
      import { syncWithClient } from ${JSON.stringify(collector)};
      const client={async callTool(name) {
        if(name==='list_threads') return {content:[{type:'text',text:JSON.stringify({threads:[{id:'thread',title:'synthetic',kind:'chatgpt',status:'idle',updatedAt:30}]})}]};
        throw new Error('429 Too many requests');
      }};
      console.log(JSON.stringify(await syncWithClient(client,'context')));
    `], { encoding: "utf8", env: { ...process.env, CHAT_HISTORY_DATA_HOME: root } });
    assert.equal(child.status, 0, child.stderr);
    const result = JSON.parse(child.stdout);
    assert.equal(result.event, "chatgpt_live_rate_limited");
    const calls = fs.readFileSync(callsPath,"utf8").trim().split("\n").map(JSON.parse);
    assert.equal(calls.some((args) => args[0] === "chatgpt-record-repair-failure"), false);
    const errorLog = path.join(root,"logs/chatgpt-live-collector.error.log");
    assert.equal(fs.existsSync(errorLog), false);
  } finally {
    fs.rmSync(root,{recursive:true,force:true});
  }
});

test("memory auto compiler is disabled by default and validates explicit bounds", () => {
  assert.deepEqual(memoryAutoConfig({}), {
    enabled: false,
    projects: [],
    scanLimit: 500,
    maxConversations: 1,
    maxMessages: 8,
    maxPendingCandidates: 20,
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
      maxPendingCandidates: 20,
      model: "gpt-5.6-sol",
    },
  );
  assert.deepEqual(
    memoryAutoConfig({
      CHAT_HISTORY_MEMORY_AUTO_PROJECTS: "LEMonX",
      CHAT_HISTORY_MEMORY_MODEL: "gpt-5.6-sol",
      CHAT_HISTORY_MEMORY_AUTO_MAX_PENDING_CANDIDATES: "8",
    }).maxPendingCandidates,
    8,
  );
  assert.throws(
    () => memoryAutoConfig({
      CHAT_HISTORY_MEMORY_AUTO_PROJECTS: "LEMonX",
      CHAT_HISTORY_MEMORY_MODEL: "gpt-5.6-sol",
      CHAT_HISTORY_MEMORY_AUTO_MAX_PENDING_CANDIDATES: "7",
    }),
    /must be an integer between 8 and 100/,
  );
  assert.throws(
    () => memoryAutoConfig({
      CHAT_HISTORY_MEMORY_AUTO_PROJECTS: "LEMonX",
      CHAT_HISTORY_MEMORY_MODEL: "gpt-5.6-sol",
      CHAT_HISTORY_MEMORY_AUTO_MAX_CONVERSATIONS: "0",
    }),
    /must be an integer between 1 and 10/,
  );
  assert.throws(
    () => memoryAutoConfig({
      CHAT_HISTORY_MEMORY_AUTO_PROJECTS: "LEMonX",
      CHAT_HISTORY_MEMORY_MODEL: "gpt-5.6-sol",
      CHAT_HISTORY_MEMORY_AUTO_MAX_PENDING_CANDIDATES: "101",
    }),
    /must be an integer between 8 and 100/,
  );
});

test("memory auto compiler health gate blocks state-integrity problems but keeps evidence warnings advisory", () => {
  const healthyWithWarnings = memoryAutoHealthGate({
    checkpoint_prefix_problem: 0,
    pending_revalidation_problem_count: 0,
    candidates: { pending: 3 },
    incomplete_canonical_conversation_count: 2,
    active_stale_or_unverified: 1,
    tracked_rejected_lower_quality_snapshots: 4,
  }, 20, 1);
  assert.equal(healthyWithWarnings.ok, true);
  assert.deepEqual(healthyWithWarnings.blockers, []);
  assert.deepEqual(
    healthyWithWarnings.warnings.map((warning) => warning.code),
    [
      "incomplete_canonical_evidence",
      "stale_or_unverified_active_memory",
      "rejected_lower_quality_evidence",
    ],
  );

  const blocked = memoryAutoHealthGate({
    checkpoint_prefix_problem: 1,
    pending_revalidation_problem_count: 2,
    candidates: { pending: 13 },
  }, 20, 1);
  assert.equal(blocked.ok, false);
  assert.deepEqual(
    blocked.blockers.map((blocker) => blocker.code),
    [
      "checkpoint_prefix_problem",
      "pending_revalidation_problem",
      "pending_candidate_backlog",
    ],
  );
  assert.deepEqual(blocked.blockers[2], {
    code: "pending_candidate_backlog",
    count: 13,
    limit: 20,
    max_new_candidates: 8,
    required_headroom: 8,
  });
  const exactHeadroom = memoryAutoHealthGate({
    checkpoint_prefix_problem: 0,
    pending_revalidation_problem_count: 0,
    candidates: { pending: 12 },
  }, 20, 1);
  assert.equal(exactHeadroom.ok, true);
  const multiAttemptBlocked = memoryAutoHealthGate({
    checkpoint_prefix_problem: 0,
    pending_revalidation_problem_count: 0,
    candidates: { pending: 5 },
  }, 20, 2);
  assert.equal(multiAttemptBlocked.ok, false);
  assert.deepEqual(multiAttemptBlocked.blockers[0], {
    code: "pending_candidate_backlog",
    count: 5,
    limit: 20,
    max_new_candidates: 16,
    required_headroom: 16,
  });
  assert.throws(
    () => memoryAutoHealthGate({
      checkpoint_prefix_problem: "not-a-number",
      pending_revalidation_problem_count: 0,
      candidates: { pending: 0 },
    }, 20, 1),
    /invalid memory health count: checkpoint_prefix_problem/,
  );
  assert.throws(
    () => memoryAutoHealthGate({
      checkpoint_prefix_problem: 0,
      pending_revalidation_problem_count: 0,
      candidates: {},
    }, 20, 1),
    /invalid memory health count: candidates\.pending/,
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
    maybeScheduleMemoryCompiler({ imported: 0, conversation_ids: [] }, fakeSpawn, enabledEnv),
    { enabled: true, scheduled: false },
  );
  assert.deepEqual(
    maybeScheduleMemoryCompiler({ imported: 2 }, fakeSpawn, enabledEnv),
    {
      enabled: true,
      scheduled: false,
      reason: "no_imported_conversation_ids",
    },
  );
  const scheduled = maybeScheduleMemoryCompiler({
    imported: 2,
    conversation_ids: ["thread-1", "thread-2", "thread-1"],
  }, fakeSpawn, enabledEnv);
  assert.equal(scheduled.enabled, true);
  assert.equal(scheduled.scheduled, true);
  assert.equal(scheduled.pid, 12345);
  assert.deepEqual(scheduled.projects, ["LEMonX"]);
  assert.equal(scheduled.model, "gpt-5.6-sol");
  assert.deepEqual(scheduled.conversation_ids, ["thread-1", "thread-2"]);
  assert.equal(calls.length, 1);
  assert.deepEqual(calls[0].args.slice(-1), ["--memory-compiler-worker"]);
  assert.equal(calls[0].options.detached, true);
  assert.equal(calls[0].options.stdio, "ignore");
  assert.equal(
    calls[0].options.env.CHAT_HISTORY_MEMORY_AUTO_TRIGGER_CONVERSATIONS,
    JSON.stringify(["thread-1", "thread-2"]),
  );
});

test("memory auto compiler trigger ids are bounded, deduplicated, and fail closed", () => {
  assert.deepEqual(memoryAutoTriggerConversationIds({
    CHAT_HISTORY_MEMORY_AUTO_TRIGGER_CONVERSATIONS: JSON.stringify([
      "thread-1",
      " thread-2 ",
      "thread-1",
      "",
    ]),
  }), ["thread-1", "thread-2"]);
  assert.throws(
    () => memoryAutoTriggerConversationIds({
      CHAT_HISTORY_MEMORY_AUTO_TRIGGER_CONVERSATIONS: "not-json",
    }),
    /must be a JSON array/,
  );
});

test("memory auto compiler scheduling errors do not escape into ingestion", () => {
  const logged = [];
  const result = safeScheduleMemoryCompiler(
    { imported: 1, conversation_ids: ["thread-1"] },
    () => {
      throw new Error("spawn failed");
    },
    {
      CHAT_HISTORY_MEMORY_AUTO_PROJECTS: "LEMonX",
      CHAT_HISTORY_MEMORY_MODEL: "gpt-5.6-sol",
    },
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
            staged: [{ candidate_ids: ["one", "two"] }],
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
        `if [ "$1" = "memory-health" ]; then printf '%s\\n' '${JSON.stringify({
          checkpoint_prefix_problem: 0,
          pending_revalidation_problem_count: 0,
          candidates: { pending: 0 },
          incomplete_canonical_conversation_count: 0,
          active_stale_or_unverified: 0,
          tracked_rejected_lower_quality_snapshots: 0,
        })}'; elif [ "$1" = "memory-project-match" ]; then printf '%s\\n' '${JSON.stringify({
          strong_match: true,
        })}'; elif [ "$1" = "memory-compile-conversation" ]; then printf '%s\\n' '${JSON.stringify({
          status: "staged",
          result: {
            input: { source_snapshot_id: "snapshot-1", through_turn_index: 5 },
            staged: { candidate_ids: ["candidate-1"] },
          },
        })}'; else exit 99; fi`,
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
        CHAT_HISTORY_MEMORY_AUTO_TRIGGER_CONVERSATIONS: JSON.stringify(["thread-1"]),
      },
    });
    assert.equal(run.status, 0, run.stderr);
    const calls = fs.readFileSync(callsPath, "utf8").trim().split("\n");
    assert.deepEqual(calls, [
      "memory-health --project LEMonX",
      "memory-project-match --project LEMonX thread-1",
      "memory-compile-conversation --project LEMonX --max-messages 6 thread-1",
    ]);
    const status = JSON.parse(
      fs.readFileSync(path.join(cache, "memory-auto-compiler-status.json"), "utf8"),
    );
    assert.equal(status.state, "completed");
    assert.deepEqual(status.projects, ["LEMonX"]);
    assert.equal(status.model, "gpt-5.6-sol");
    assert.equal(status.results.length, 1);
    assert.equal(status.results[0].status, "ok");
    assert.equal(status.results[0].result.model_attempts, 1);
    assert.equal(status.results[0].result.staged[0].conversation_id, "thread-1");
    assert.deepEqual(status.trigger_conversation_ids, ["thread-1"]);
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

test("memory compiler worker blocks unhealthy projects before model compilation without recording a worker failure", () => {
  const root = fs.mkdtempSync(path.join(os.tmpdir(), "chat-history-memory-health-gate-"));
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
        `if [ "$1" = "memory-health" ]; then printf '%s\\n' '${JSON.stringify({
          checkpoint_prefix_problem: 1,
          pending_revalidation_problem_count: 0,
          candidates: { pending: 0 },
          incomplete_canonical_conversation_count: 1,
          active_stale_or_unverified: 0,
          tracked_rejected_lower_quality_snapshots: 0,
        })}'; else echo "compiler must not run" >&2; exit 99; fi`,
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
        CHAT_HISTORY_MEMORY_MODEL: "gpt-5.6-sol",
        CHAT_HISTORY_MEMORY_AUTO_TRIGGER_CONVERSATIONS: JSON.stringify(["thread-1"]),
      },
    });
    assert.equal(run.status, 0, run.stderr);
    const calls = fs.readFileSync(callsPath, "utf8").trim().split("\n");
    assert.deepEqual(calls, ["memory-health --project LEMonX"]);
    const status = JSON.parse(
      fs.readFileSync(path.join(cache, "memory-auto-compiler-status.json"), "utf8"),
    );
    assert.equal(status.state, "completed");
    assert.equal(status.results[0].status, "blocked_by_health");
    assert.equal(status.results[0].health.blockers[0].code, "checkpoint_prefix_problem");
    assert.equal(status.results[0].health.warnings[0].code, "incomplete_canonical_evidence");
    const history = JSON.parse(
      fs.readFileSync(path.join(cache, "memory-auto-compiler-history.json"), "utf8"),
    );
    assert.equal(history.total_runs, 1);
    assert.equal(history.consecutive_failures, 0);
    assert.equal(history.last_failure_at, null);
    assert.equal(history.runs[0].projects[0].status, "blocked_by_health");
    assert.deepEqual(history.runs[0].projects[0].blockers, [{
      code: "checkpoint_prefix_problem",
      count: 1,
    }]);
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
