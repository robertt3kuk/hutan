// hutan-managed: OMP agent-state extension.
// Installed by `hutan integration install omp`; reinstalling overwrites this file, so put
// custom extensions beside it instead of editing it.
//
// Reports OMP's state (turn running, waiting for an approval or an answer, idle) and how to
// resume the conversation (`omp --resume=<id>`) to the Hutan session OMP runs in, by calling
// `hutan agent report`. Every agent is a node of the session's agent tree: OMP binds this
// extension to the main session and to every subagent session, and `ctx.agent` (`kind`, `id`,
// `parentId`, `name`) says which one an instance serves. Does nothing outside a Hutan session.
// The main agent also reports its insight (tokens per model call, cost, tools with times, files
// from the edit tools' arguments, the context and its fixed part): see "insight" at the end.
// @ts-nocheck

import { spawn } from "node:child_process";
import { existsSync } from "node:fs";

const SOURCE = "omp-extension";
// A turn that ends and restarts within this time (an automatic retry) stays "working".
const IDLE_DEBOUNCE_MS = 400;
// Ids that are safe inside the resume command the daemon will run unattended.
const SAFE_ID = /^[A-Za-z0-9._:-]{1,200}$/;

type State = "idle" | "working" | "blocked";

interface HostContext {
  hasUI?: boolean;
  isIdle?: () => boolean;
  sessionManager?: { getSessionId?: () => unknown };
  agent?: { kind?: string; id?: string; parentId?: string; name?: string };
  model?: { id?: string; name?: string };
}

interface Identity {
  kind: "main" | "sub";
  id: string;
  parentId?: string;
  name?: string;
}

interface ApprovalEvent {
  reason?: string;
  toolName?: string;
}

interface ToolEvent {
  toolName?: string;
  args?: { questions?: { question?: unknown }[] };
}

const ENABLED = process.env.HUTAN_ENV === "1" && !!process.env.HUTAN_SESSION_ID;

// Increases per report; microseconds keep it above what an earlier instance of this
// extension (a reload) used and fit a double exactly.
let lastSeq = Date.now() * 1000;
function nextSeq(): number {
  lastSeq += 1;
  return lastSeq;
}

// One `hutan` process at a time, in the order of the events, each bounded in time.
let queue: Promise<void> = Promise.resolve();
function hutan(args: string[]): void {
  queue = queue.then(
    () =>
      new Promise<void>((resolve) => {
        let done = false;
        const finish = () => {
          if (done) return;
          done = true;
          resolve();
        };
        try {
          const child = spawn(process.env.HUTAN_BIN || "hutan", args, {
            stdio: "ignore",
            windowsHide: true,
          });
          child.on("error", finish);
          child.on("exit", finish);
          setTimeout(finish, 1500).unref?.();
        } catch {
          finish();
        }
      }),
  );
}

function report(
  who: Identity,
  state: State,
  message?: string,
  model?: string,
  agentSessionId?: string,
  outcome?: string,
): void {
  // `--name=value` so a value that starts with `-` is not taken for an option.
  const args = [
    "agent",
    "report",
    "--source",
    SOURCE,
    "--agent",
    "omp",
    "--state",
    state,
    `--seq=${nextSeq()}`,
    `--node=${who.id}`,
    `--kind=${who.kind}`,
  ];
  if (who.parentId) args.push(`--parent=${who.parentId}`);
  if (who.name) args.push(`--name=${who.name}`);
  if (model) args.push(`--model=${model}`);
  if (outcome) args.push(`--outcome=${outcome}`);
  if (message) args.push(`--message=${message.slice(0, 200)}`);
  // Only the main agent's conversation can be resumed.
  if (who.kind === "main" && agentSessionId) {
    args.push(`--agent-session-id=${agentSessionId}`, "--", "omp", `--resume=${agentSessionId}`);
  }
  hutan(args);
}

export default function (pi) {
  if (!ENABLED) return;

  let identity: Identity | undefined;
  let model: string | undefined;
  let agentSessionId: string | undefined;
  let rootSession = false;
  let turnActive = false;
  let blockedCount = 0;
  let blockedLabel: string | undefined;
  let lastPublished: string | undefined;
  let idleTimer: NodeJS.Timeout | undefined;

  function clearIdleTimer() {
    clearTimeout(idleTimer);
    idleTimer = undefined;
  }

  function publish(force = false) {
    let state: State = "idle";
    let message: string | undefined;
    if (blockedCount > 0) {
      state = "blocked";
      message = blockedLabel;
    } else if (turnActive) {
      state = "working";
    }
    // A subagent whose agent loop is over has finished its task.
    const outcome = identity?.kind === "sub" && state === "idle" ? "completed" : undefined;
    const key = `${state}|${message ?? ""}|${model ?? ""}|${outcome ?? ""}`;
    if (!force && key === lastPublished) return;
    lastPublished = key;
    if (identity) report(identity, state, message, model, agentSessionId, outcome);
  }

  // This instance serves either the interactive (main) session, which the user sees, or a
  // subagent session (`ctx.agent.kind === "sub"`). Print mode has no UI and is neither.
  function adopt(ctx: HostContext | undefined): boolean {
    const agent = ctx?.agent;
    const safe = (value: unknown): value is string =>
      typeof value === "string" && SAFE_ID.test(value);
    const label = (value: unknown) =>
      typeof value === "string" && value.length > 0 ? value.slice(0, 100) : undefined;
    const known = ctx?.model?.id ?? ctx?.model?.name;
    model = typeof known === "string" && known.length <= 128 ? known : model;
    if (agent?.kind === "sub" && safe(agent.id)) {
      identity = {
        kind: "sub",
        id: agent.id,
        parentId: safe(agent.parentId) ? agent.parentId : undefined,
        name: label(agent.name),
      };
      rootSession = true;
      return true;
    }
    if (ctx?.hasUI !== true) return false;
    identity = { kind: "main", id: safe(agent?.id) ? agent.id : "Main", name: label(agent?.name) };
    rootSession = true;
    try {
      const id = ctx.sessionManager?.getSessionId?.();
      agentSessionId = typeof id === "string" && SAFE_ID.test(id) ? id : undefined;
    } catch {
      agentSessionId = undefined;
    }
    return true;
  }

  function block(label: string | undefined) {
    clearIdleTimer();
    blockedCount += 1;
    blockedLabel = label;
    publish();
  }

  function unblock() {
    blockedCount = Math.max(0, blockedCount - 1);
    if (blockedCount === 0) blockedLabel = undefined;
    publish();
  }

  pi.on("session_start", (_event: unknown, ctx: HostContext) => {
    if (!adopt(ctx)) return;
    // A reload can replace this extension in the middle of a turn.
    turnActive = ctx.isIdle?.() === false;
    publish(true);
  });

  pi.on("session_switch", (_event: unknown, ctx: HostContext) => {
    if (!adopt(ctx)) return;
    clearIdleTimer();
    turnActive = false;
    blockedCount = 0;
    blockedLabel = undefined;
    publish(true);
  });

  pi.on("agent_start", (_event: unknown, ctx: HostContext) => {
    if (!adopt(ctx)) return;
    clearIdleTimer();
    turnActive = true;
    publish();
  });

  pi.on("agent_end", (event: { willContinue?: boolean }) => {
    // The loop goes on (a retry, queued follow-up work): still working.
    if (event?.willContinue === true) return;
    if (!rootSession || !turnActive) return;
    turnActive = false;
    clearIdleTimer();
    idleTimer = setTimeout(() => {
      idleTimer = undefined;
      publish();
    }, IDLE_DEBOUNCE_MS);
    idleTimer.unref?.();
  });

  pi.on("tool_approval_requested", (event: ApprovalEvent, ctx: HostContext) => {
    if (!rootSession && !adopt(ctx)) return;
    block(event?.reason || `${event?.toolName || "tool"} approval`);
  });

  pi.on("tool_approval_resolved", () => {
    if (!rootSession) return;
    unblock();
  });

  // The `ask` tool waits for the user's answer.
  pi.on("tool_execution_start", (event: ToolEvent, ctx: HostContext) => {
    if (event?.toolName !== "ask") return;
    if (!rootSession && !adopt(ctx)) return;
    const questions = Array.isArray(event.args?.questions) ? event.args.questions : [];
    const first = questions.find((q) => typeof q?.question === "string");
    block(typeof first?.question === "string" ? first.question : "waiting for an answer");
  });

  pi.on("tool_execution_end", (event: ToolEvent) => {
    if (event?.toolName !== "ask" || !rootSession) return;
    unblock();
  });

  pi.on("session_shutdown", () => {
    clearIdleTimer();
    // A subagent's session closing ends its node, whatever state it was in.
    if (identity?.kind === "sub") {
      lastPublished = undefined;
      turnActive = false;
      blockedCount = 0;
      publish(true);
    }
  });

  // Tokens, tools, files and context of the main agent (see the end of this file).
  trackInsight(pi, () => identity);
}

// ---- insight: tokens, tools, files and context for the Context panel --------------------------
//
// Reports what OMP's extension API gives about the main agent's conversation (`hutan agent report
// --insight-json`, protocol 0.72): the usage of every model call, the tools that finished with
// their times, the files the edit/write tools changed (from the tools' arguments), the context
// and its fixed part at the start of a conversation. One report at the end of the agent loop and
// at most one every INSIGHT_GAP_MS while it runs. Counts, tool names and paths only: no prompt,
// no tool output, no diff. Subagents report nothing of their own; a failed or slow `hutan` loses
// the delta it carried.

const INSIGHT_GAP_MS = 500;
// A report is at most 4 KiB; leave room for the envelope.
const INSIGHT_MAX_JSON = 3600;
const MAX_ROWS = 16;
const MAX_FILES = 32;
// Tools that change files.
const FILE_TOOLS = new Set(["edit", "write", "ast_edit", "notebook", "delete"]);

type FileKind = "add" | "update" | "delete";

interface InsightContext {
  used: number;
  window?: number;
  fixed?: number;
}

interface PendingInsight {
  turn?: { phase: "start" | "end"; outcome?: string; duration_ms?: number };
  tokens?: Record<string, number>;
  context?: InsightContext;
  cost_usd?: number;
  tools: Map<string, { calls: number; failed: number; ms: number }>;
  files: Map<string, FileKind>;
}

interface InsightHost {
  getContextUsage?: () => { tokens?: unknown; contextWindow?: unknown } | undefined;
  sessionManager?: {
    getUsageStatistics?: () => { cost?: unknown } | undefined;
    getEntries?: () => unknown;
  };
  model?: { id?: string };
}

interface InsightEvent {
  toolCallId?: unknown;
  toolName?: unknown;
  args?: unknown;
  isError?: unknown;
  willContinue?: unknown;
  messages?: unknown;
  message?: { role?: unknown; usage?: Record<string, unknown> };
}

function emptyInsight(): PendingInsight {
  return { tools: new Map(), files: new Map() };
}

function count(value: unknown): number | undefined {
  return typeof value === "number" && Number.isFinite(value) && value >= 0 ? Math.round(value) : undefined;
}

function record(value: unknown): Record<string, unknown> | undefined {
  return value !== null && typeof value === "object" ? (value as Record<string, unknown>) : undefined;
}

// The files a file tool names: `path` / `file_path`, the `paths` of `ast_edit`, or the
// `[path#tag]` headers of a hashline patch (`input`).
function pathsOf(toolName: string, args: unknown): string[] {
  const named = record(args);
  if (!FILE_TOOLS.has(toolName) || !named) return [];
  const found: string[] = [];
  const add = (path: unknown) => {
    if (typeof path === "string" && path.length > 0 && path.length <= 1024 && !/[\u0000-\u001f]/.test(path)) {
      found.push(path);
    }
  };
  add(named.path);
  add(named.file_path);
  add(named.filePath);
  if (Array.isArray(named.paths)) for (const path of named.paths.slice(0, MAX_FILES)) add(path);
  if (typeof named.input === "string") {
    for (const line of named.input.split("\n", 400)) {
      const header = /^\[(.+?)#[0-9A-Za-z]{4}\]$/.exec(line);
      if (header) add(header[1]);
    }
  }
  return found;
}

function trackInsight(pi, identityOf: () => Identity | undefined) {
  let pending = emptyInsight();
  let flushTimer: NodeJS.Timeout | undefined;
  let lastFlush = 0;
  let turnStartedAt = 0;
  let measuredModel: string | undefined;
  const started = new Map<string, { name: string; at: number; args: unknown; existed: Map<string, boolean> }>();

  const on = (event: string, handler: (event: InsightEvent, ctx: InsightHost) => void) => {
    try {
      pi.on(event, (e: InsightEvent, ctx: InsightHost) => {
        try {
          if (identityOf()?.kind === "main") handler(e ?? {}, ctx ?? {});
        } catch {
          // An insight is never worth disturbing the agent for.
        }
      });
    } catch {
      // This OMP has no such event.
    }
  };

  const contextOf = (ctx: InsightHost, fixed: boolean): InsightContext | undefined => {
    const usage = ctx.getContextUsage?.();
    const used = count(usage?.tokens);
    if (used === undefined) return undefined;
    const context: InsightContext = { used };
    const window = count(usage?.contextWindow);
    if (window) context.window = window;
    if (fixed) context.fixed = used;
    return context;
  };

  const costOf = (ctx: InsightHost): number | undefined => {
    const cost = ctx.sessionManager?.getUsageStatistics?.()?.cost;
    return typeof cost === "number" && Number.isFinite(cost) && cost >= 0 ? cost : undefined;
  };

  // The conversation is empty: what the context holds now is all fixed (the system prompt, the
  // tools, the project files).
  const emptyConversation = (ctx: InsightHost): boolean => {
    const entries = ctx.sessionManager?.getEntries?.();
    return Array.isArray(entries) && !entries.some((entry) => record(entry)?.type === "message");
  };

  function payload(): Record<string, unknown> | undefined {
    const insight: Record<string, unknown> = { source: "extension" };
    if (pending.turn) insight.turn = pending.turn;
    if (pending.tokens) insight.tokens = pending.tokens;
    if (pending.context) insight.context = pending.context;
    if (pending.cost_usd !== undefined) insight.cost_usd = pending.cost_usd;
    const tools = [...pending.tools];
    if (tools.length) {
      insight.tools = tools.slice(0, MAX_ROWS).map(([name, row]) => ({
        name,
        calls: row.calls,
        ...(row.failed ? { failed: row.failed } : {}),
        ...(row.ms ? { ms: row.ms } : {}),
      }));
    }
    const files = [...pending.files];
    const sent = new Set<string>();
    const listed: { path: string; kind: FileKind }[] = [];
    insight.files = listed;
    for (const [path, kind] of files.slice(0, MAX_FILES)) {
      listed.push({ path, kind });
      if (JSON.stringify(insight).length > INSIGHT_MAX_JSON) {
        listed.pop();
        break;
      }
      sent.add(path);
    }
    if (!listed.length) delete insight.files;
    if (Object.keys(insight).length === 1) return undefined;
    // What was sent is gone; the rest waits for the next report.
    const rest = emptyInsight();
    for (const [name, row] of tools.slice(MAX_ROWS)) rest.tools.set(name, row);
    for (const [path, kind] of files) if (!sent.has(path)) rest.files.set(path, kind);
    pending = rest;
    return insight;
  }

  function flush() {
    clearTimeout(flushTimer);
    flushTimer = undefined;
    lastFlush = Date.now();
    const insight = payload();
    if (!insight) return;
    hutan([
      "agent",
      "report",
      "--source",
      SOURCE,
      "--agent",
      "omp",
      `--seq=${nextSeq()}`,
      `--insight-json=${JSON.stringify(insight)}`,
    ]);
    // Something did not fit: the next report carries it.
    if (pending.tools.size || pending.files.size) schedule();
  }

  function schedule() {
    if (flushTimer) return;
    const wait = Math.max(0, INSIGHT_GAP_MS - (Date.now() - lastFlush));
    flushTimer = setTimeout(flush, wait);
    flushTimer.unref?.();
  }

  // The context at the start of a conversation is fixed overhead; read again, with the cost, when
  // OMP switches conversation, compacts or changes the model.
  const measure = (_event: InsightEvent, ctx: InsightHost) => {
    started.clear();
    const context = contextOf(ctx, emptyConversation(ctx));
    if (context) pending.context = context;
    const cost = costOf(ctx);
    if (cost !== undefined) pending.cost_usd = cost;
    measuredModel = ctx.model?.id;
    flush();
  };
  on("session_start", measure);
  on("session_switch", measure);
  on("session_compact", (_event, ctx) => {
    const context = contextOf(ctx, true);
    if (context) pending.context = context;
    flush();
  });
  on("model_select", (_event, ctx) => {
    if (ctx.model?.id === measuredModel) return;
    measuredModel = ctx.model?.id;
    const context = contextOf(ctx, emptyConversation(ctx));
    if (context) pending.context = context;
    schedule();
  });

  on("agent_start", () => {
    turnStartedAt = Date.now();
    pending.turn = { phase: "start" };
    schedule();
  });

  // Every model call: its usage is a delta of the running turn.
  on("message_end", (event) => {
    const message = event.message;
    const usage = message?.usage;
    if (message?.role !== "assistant" || !usage) return;
    const sum = (key: string, value: unknown) => {
      const n = count(value);
      if (n === undefined) return;
      pending.tokens = pending.tokens ?? {};
      pending.tokens[key] = (pending.tokens[key] ?? 0) + n;
    };
    sum("input_tokens", usage.input);
    sum("output_tokens", usage.output);
    sum("cached_input_tokens", usage.cacheRead);
    sum("cache_write_tokens", usage.cacheWrite);
    sum("total_tokens", usage.totalTokens);
    schedule();
  });

  on("tool_execution_start", (event) => {
    if (typeof event.toolCallId !== "string" || typeof event.toolName !== "string") return;
    // Whether a file a `write` is about to create is there already: a new file is `add`.
    const existed = new Map<string, boolean>();
    if (event.toolName === "write") {
      for (const path of pathsOf("write", event.args)) {
        try {
          existed.set(path, existsSync(path));
        } catch {
          // Unknown: counted as an update.
        }
      }
    }
    if (started.size > 256) started.clear();
    started.set(event.toolCallId, { name: event.toolName, at: Date.now(), args: event.args, existed });
  });

  on("tool_execution_end", (event) => {
    const call = typeof event.toolCallId === "string" ? started.get(event.toolCallId) : undefined;
    if (typeof event.toolCallId === "string") started.delete(event.toolCallId);
    const name = typeof event.toolName === "string" ? event.toolName : call?.name;
    if (!name) return;
    const row = pending.tools.get(name) ?? { calls: 0, failed: 0, ms: 0 };
    row.calls += 1;
    if (event.isError === true) row.failed += 1;
    if (call) row.ms += Math.max(0, Date.now() - call.at);
    pending.tools.set(name, row);
    if (call && event.isError !== true) {
      for (const path of pathsOf(name, call.args)) {
        const kind: FileKind = name === "delete" ? "delete" : name === "write" && call.existed.get(path) === false ? "add" : "update";
        pending.files.set(path, kind);
      }
    }
    schedule();
  });

  on("agent_end", (event, ctx) => {
    // A retry or queued work goes on: still the same turn.
    if (event.willContinue === true) {
      schedule();
      return;
    }
    const messages = Array.isArray(event.messages) ? event.messages : [];
    const last = [...messages].reverse().find((message) => record(message)?.role === "assistant");
    const stop = record(last)?.stopReason;
    const outcome = stop === "error" ? "failed" : stop === "aborted" ? "cancelled" : undefined;
    // The start is still waiting: it goes first, so the report that ends the turn ends something.
    if (pending.turn?.phase === "start") flush();
    pending.turn = {
      phase: "end",
      ...(outcome ? { outcome } : {}),
      ...(turnStartedAt ? { duration_ms: Math.max(0, Date.now() - turnStartedAt) } : {}),
    };
    const context = contextOf(ctx, false);
    if (context) pending.context = context;
    const cost = costOf(ctx);
    if (cost !== undefined) pending.cost_usd = cost;
    flush();
  });
}
