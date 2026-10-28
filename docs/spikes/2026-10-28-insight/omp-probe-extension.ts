// Probe extension for the INSIGHT-SPIKE (OMP 18.12.3): logs what an extension can read about
// usage. Not shipped; load it with `omp -e`. Output file: $PROBE_OUT (JSON lines).
import { appendFileSync } from "node:fs";

const OUT = process.env.PROBE_OUT ?? "probe.out";

export default function (pi) {
  const log = (k, v) => appendFileSync(OUT, JSON.stringify({ k, t: Date.now(), v }) + "\n");
  const strip = (m) =>
    m && {
      role: m.role,
      model: m.model,
      provider: m.provider,
      stopReason: m.stopReason,
      usage: m.usage,
      contextSnapshot: m.contextSnapshot,
      toolCalls: Array.isArray(m.content)
        ? m.content.filter((c) => c.type === "toolCall").map((c) => ({ name: c.name, id: c.id }))
        : undefined,
    };
  pi.on("session_start", (_e, ctx) => {
    log("ctx.methods", Object.getOwnPropertyNames(Object.getPrototypeOf(ctx) ?? {}).sort());
    log("getContextUsage@start", ctx.getContextUsage?.());
    log("systemPrompt.chars", (ctx.getSystemPrompt?.() ?? []).map((s) => s.length));
    log("getContextBreakdown", typeof ctx.getContextBreakdown);
  });
  pi.on("turn_start", (e) => log("turn_start", Object.keys(e)));
  pi.on("turn_end", (e, ctx) =>
    log("turn_end", {
      keys: Object.keys(e),
      message: strip(e.message),
      toolResults: (e.toolResults || []).map((r) => ({ toolName: r.toolName, isError: r.isError })),
      ctxUsage: ctx.getContextUsage?.(),
    }),
  );
  pi.on("message_end", (e) => log("message_end", strip(e.message)));
  pi.on("tool_execution_start", (e) => log("tool_execution_start", { keys: Object.keys(e), toolName: e.toolName }));
  pi.on("tool_execution_end", (e) =>
    log("tool_execution_end", { keys: Object.keys(e), toolName: e.toolName, isError: e.isError }),
  );
  pi.on("agent_end", (e, ctx) => {
    let stats;
    try {
      stats = ctx.sessionManager.getUsageStatistics();
    } catch {}
    log("agent_end", { keys: Object.keys(e), stats, ctxUsage: ctx.getContextUsage?.() });
  });
}
