// Unit test del punto unico di composizione del nastro attivita' (ADR 0037).
// Runner: `node --test` con type-stripping nativo (Node >= 22.18 / 24). Import
// con estensione .ts esplicita, richiesta dal loader ESM di Node. Il typecheck
// di questi file usa tsconfig.test.json (allowImportingTsExtensions); il
// tsconfig principale di Next li esclude.
//
// Copertura richiesta dal task:
//  - cambio provider (escalation) apre un nuovo segmento;
//  - collasso alla soglia densita' (>= N tool ok consecutivi);
//  - esito tool derivato dal segnale strutturato (status), non dal testo.

import { test } from "node:test";
import assert from "node:assert/strict";
import {
  composeActivityStream,
  foldConsecutiveOkTools,
  aggregateTokensByProvider,
  tracesForRun,
  capStreamToRecent,
  type ActivityEvent,
  type ToolEvent,
} from "./activity-stream.ts";
import type { MetaStepEntry } from "./types.ts";
import type { AgentStep, AITraceEvent } from "../api/agent.ts";

// ── Costruttori di fixture ──────────────────────────────────────────────────

let clock = 0;
function ts(): string {
  // Timestamp monotono crescente per un ordinamento deterministico.
  clock += 1000;
  return new Date(clock).toISOString();
}

function meta(kind: string, payload: Record<string, unknown>, title = kind): MetaStepEntry {
  return { kind, title, payload, createdAt: ts() };
}

function step(
  toolName: string,
  status: AgentStep["status"],
  stepIndex: number,
  extra: Partial<AgentStep> = {},
): AgentStep {
  return {
    stepIndex,
    toolName,
    toolInput: extra.toolInput ?? {},
    toolResult: extra.toolResult,
    status,
    createdAt: ts(),
  };
}

function trace(
  runId: string,
  iteration: number,
  provider: string,
  model: string,
  extra: Partial<AITraceEvent> = {},
): AITraceEvent {
  return {
    runId,
    iteration,
    provider,
    model,
    messagesSent: 0,
    toolsCount: 0,
    responseText: "",
    toolCalls: [],
    stopReason: "tool_use",
    timestamp: ts(),
    inputTokens: extra.inputTokens,
    outputTokens: extra.outputTokens,
    ...extra,
  };
}

function beforeEach() {
  clock = 0;
}

// ── 1. Cambio provider apre un nuovo segmento ───────────────────────────────

test("escalation apre un nuovo segmento con banda switch", () => {
  beforeEach();
  const metaSteps: MetaStepEntry[] = [
    meta("routing", { intent: "code_fix", provider: "google", model: "gemini-2.5-pro" }),
    meta("executor_call", { iteration: 1, provider: "google", model: "gemini-2.5-pro" }),
    meta("escalation", {
      from_provider: "google",
      to_provider: "anthropic",
      to_model: "claude-sonnet",
      reason: "signature_loop",
      cooldown: "quota",
    }),
    meta("executor_call", { iteration: 5, provider: "anthropic", model: "claude-sonnet" }),
  ];
  const stream = composeActivityStream(metaSteps, [], [], 3);

  assert.equal(stream.empty, false);
  assert.equal(stream.segments.length, 2, "due segmenti: google poi anthropic");

  const [first, second] = stream.segments;
  assert.equal(first.provider, "google");
  assert.equal(first.openedBySwitch, false);

  assert.equal(second.provider, "anthropic");
  assert.equal(second.openedBySwitch, true);
  assert.ok(second.switch, "il secondo segmento porta i dati dello switch");
  assert.equal(second.switch?.fromProvider, "google");
  assert.equal(second.switch?.toProvider, "anthropic");
  assert.equal(second.switch?.reason, "signature_loop");
  assert.equal(second.switch?.cooldown, "quota");
});

test("fallback senza to_provider strutturato non crea segmento fantasma", () => {
  beforeEach();
  const metaSteps: MetaStepEntry[] = [
    meta("executor_call", { iteration: 1, provider: "openai", model: "gpt-4o" }),
    // payload privo di to_provider: degrado pulito, nessun nuovo segmento.
    meta("fallback", { reason: "billing_error" }),
  ];
  const stream = composeActivityStream(metaSteps, [], [], 3);
  assert.equal(stream.segments.length, 1);
  assert.equal(stream.segments[0].provider, "openai");
});

// ── 2. Collasso alla soglia densita' ────────────────────────────────────────

test("collasso: >= soglia tool ok consecutivi comprimono in folded_tools", () => {
  beforeEach();
  const metaSteps: MetaStepEntry[] = [meta("executor_call", { iteration: 0, provider: "anthropic", model: "claude" })];
  const steps: AgentStep[] = [
    step("read_file", "completed", 0),
    step("edit_file", "completed", 1),
    step("read_file", "completed", 2),
    step("edit_file", "completed", 3),
  ];
  // soglia 3: 4 tool ok consecutivi -> un solo folded_tools(count=4).
  const s3 = composeActivityStream(metaSteps, steps, [], 3);
  const events3 = s3.segments[0].events;
  const folded = events3.find((e) => e.type === "folded_tools");
  assert.ok(folded, "esiste un evento folded_tools");
  assert.equal(folded.type === "folded_tools" ? folded.count : -1, 4);
  assert.equal(events3.filter((e) => e.type === "tool").length, 0, "nessun tool espanso");
  // Il folded CONSERVA i ToolEvent originali (espandibili dal renderer):
  // niente troncamento silenzioso.
  assert.ok(folded.type === "folded_tools");
  assert.equal(folded.tools.length, 4, "i 4 tool sono conservati in tools");
  assert.ok(folded.tools.every((t) => t.type === "tool" && t.outcome === "ok"));
});

test("collasso: sequenza sotto soglia resta espansa", () => {
  beforeEach();
  const metaSteps: MetaStepEntry[] = [meta("executor_call", { iteration: 0, provider: "anthropic", model: "claude" })];
  const steps: AgentStep[] = [
    step("read_file", "completed", 0),
    step("edit_file", "completed", 1),
  ];
  // soglia 3: solo 2 tool ok -> restano espansi, niente folding.
  const s = composeActivityStream(metaSteps, steps, [], 3);
  const events = s.segments[0].events;
  assert.equal(events.filter((e) => e.type === "folded_tools").length, 0);
  assert.equal(events.filter((e) => e.type === "tool").length, 2);
});

test("collasso: soglia 2 (compatto) e' piu' aggressiva di soglia 4 (esteso)", () => {
  beforeEach();
  const evs: ActivityEvent[] = [
    { type: "tool", name: "a", outcome: "ok", iteration: 0 },
    { type: "tool", name: "b", outcome: "ok", iteration: 1 },
    { type: "tool", name: "c", outcome: "ok", iteration: 2 },
  ];
  const compatto = foldConsecutiveOkTools(evs, 2);
  assert.equal(compatto.length, 1, "3 tool ok con soglia 2 -> tutti collassati");
  assert.equal(compatto[0].type, "folded_tools");
  // Il folded conserva TUTTI i tool originali (espandibili singolarmente).
  assert.equal(compatto[0].type === "folded_tools" ? compatto[0].tools.length : -1, 3);

  const esteso = foldConsecutiveOkTools(evs, 4);
  assert.equal(esteso.length, 3, "3 tool ok con soglia 4 -> restano espansi");
  assert.ok(esteso.every((e) => e.type === "tool"));
});

// ── 3. Esito tool da segnale strutturato (regola M) ─────────────────────────

test("esito tool: status failed -> outcome err e non viene collassato", () => {
  beforeEach();
  const metaSteps: MetaStepEntry[] = [meta("executor_call", { iteration: 0, provider: "anthropic", model: "claude" })];
  const steps: AgentStep[] = [
    step("read_file", "completed", 0),
    step("run", "failed", 1, { toolResult: "exit_code=101\ncompilazione fallita" }),
    step("read_file", "completed", 2),
    step("read_file", "completed", 3),
    step("read_file", "completed", 4),
  ];
  const s = composeActivityStream(metaSteps, steps, [], 3);
  const events = s.segments[0].events;

  // Il tool in errore rompe la sequenza: resta visibile come tool err.
  const errTool = events.find((e): e is ToolEvent => e.type === "tool" && e.outcome === "err");
  assert.ok(errTool, "il tool fallito resta un evento tool visibile");
  assert.equal(errTool.name, "run");
  // exit code letto dal segnale strutturato exit_code=N, non dal testo.
  assert.equal(errTool.exitCode, 101);

  // Prima dell'errore: 1 tool ok (sotto soglia) resta espanso; dopo: 3 tool ok
  // (soglia 3) collassati.
  assert.equal(events.filter((e) => e.type === "folded_tools").length, 1);
});

test("esito tool: outcome dedotto SOLO da status, mai dal testo del risultato", () => {
  beforeEach();
  // Un tool COMPLETATO il cui risultato contiene la parola 'error' resta ok:
  // l'esito viene dallo status strutturato, non dal parsing del testo.
  const steps: AgentStep[] = [
    step("run", "completed", 0, { toolResult: "no error found, all good" }),
  ];
  const s = composeActivityStream([], steps, [], 3);
  const tool = s.segments[0].events.find((e): e is ToolEvent => e.type === "tool");
  assert.ok(tool);
  assert.equal(tool.outcome, "ok", "status=completed -> ok nonostante 'error' nel testo");
});

// ── Extra: provider effettivo dalle trace + aggregazione token ──────────────

test("provider di segmento preso dalla trace effettiva quando differisce dal payload", () => {
  beforeEach();
  const runId = "run-1";
  const metaSteps: MetaStepEntry[] = [
    // Il routing indica google, ma la trace effettiva della iter 1 e' anthropic.
    meta("routing", { intent: "x", provider: "google", model: "gemini" }),
    meta("executor_call", { iteration: 1, provider: "google", model: "gemini" }),
  ];
  const traces = [trace(runId, 1, "anthropic", "claude-sonnet")];
  const stream = composeActivityStream(metaSteps, [], traces, 3);
  // executor_call adotta il provider EFFETTIVO della trace della stessa iterazione.
  const providers = stream.segments.map((s) => s.provider);
  assert.ok(providers.includes("anthropic"), "il provider effettivo dalla trace prevale");
});

test("aggregateTokensByProvider somma i token per coppia provider/model", () => {
  beforeEach();
  const runId = "run-1";
  const traces = [
    trace(runId, 1, "google", "gemini", { inputTokens: 100, outputTokens: 40 }),
    trace(runId, 2, "google", "gemini", { inputTokens: 50, outputTokens: 10 }),
    trace(runId, 3, "anthropic", "claude", { inputTokens: 200, outputTokens: 80 }),
  ];
  const buckets = aggregateTokensByProvider(traces);
  assert.equal(buckets.length, 2);
  const google = buckets.find((b) => b.provider === "google");
  assert.equal(google?.inputTokens, 150);
  assert.equal(google?.outputTokens, 50);
  const anthropic = buckets.find((b) => b.provider === "anthropic");
  assert.equal(anthropic?.inputTokens, 200);
});

test("tracesForRun filtra per runId", () => {
  beforeEach();
  const traces = [trace("a", 1, "google", "gemini"), trace("b", 1, "openai", "gpt")];
  assert.equal(tracesForRun(traces, "a").length, 1);
  assert.equal(tracesForRun(traces, "a")[0].provider, "google");
});

test("stream vuoto: nessun segnale -> empty true", () => {
  beforeEach();
  const stream = composeActivityStream([], [], [], 3);
  assert.equal(stream.empty, true);
  assert.equal(stream.segments.length, 0);
});

// ── ToolEvent porta input/result dallo step (espansione riga tool) ──────────

test("ToolEvent espone input e result dallo step", () => {
  beforeEach();
  const steps: AgentStep[] = [
    step("edit_file", "completed", 0, {
      toolInput: { path: "src/main.rs", content: "fn main() {}" },
      toolResult: "file scritto (12 righe)",
    }),
  ];
  const s = composeActivityStream([], steps, [], 3);
  const tool = s.segments[0].events.find((e): e is ToolEvent => e.type === "tool");
  assert.ok(tool);
  assert.deepEqual(tool.input, { path: "src/main.rs", content: "fn main() {}" });
  assert.equal(tool.result, "file scritto (12 righe)");
});

test("unwrap: step storico con toolName vuoto e { tool_name, tool_input } annidato", () => {
  beforeEach();
  // Forma DB (getAgentRun): toolName VUOTO, nome e parametri annidati.
  const steps: AgentStep[] = [
    step("", "completed", 0, {
      toolInput: { tool_name: "read_file", tool_input: { path: "src/x.ts" } },
      toolResult: '{"content":"riga1\\nriga2","status":"completed"}',
    }),
  ];
  const s = composeActivityStream([], steps, [], 3);
  const tool = s.segments[0].events.find((e): e is ToolEvent => e.type === "tool");
  assert.ok(tool, "il tool e' presente nonostante toolName vuoto");
  assert.equal(tool.name, "read_file", "nome ripristinato dal wrapper");
  assert.deepEqual(tool.input, { path: "src/x.ts" }, "parametri veri, senza wrapper");
  assert.equal(tool.target, "src/x.ts", "target dal parametro path reale");
});

test("unwrap: step SSE con toolName presente non viene alterato", () => {
  beforeEach();
  const steps: AgentStep[] = [
    step("edit_file", "completed", 0, { toolInput: { path: "a.ts", content: "x" } }),
  ];
  const s = composeActivityStream([], steps, [], 3);
  const tool = s.segments[0].events.find((e): e is ToolEvent => e.type === "tool");
  assert.ok(tool);
  assert.equal(tool.name, "edit_file");
  assert.deepEqual(tool.input, { path: "a.ts", content: "x" });
});

// ── Stamping provider/model per riga (icona provider) ───────────────────────

test("provenance: ogni riga porta provider/model del segmento", () => {
  beforeEach();
  const metaSteps: MetaStepEntry[] = [
    meta("routing", { intent: "fix", provider: "google", model: "gemini-2.5-pro" }),
    meta("final_gate", { phase: "passed" }),
  ];
  const steps: AgentStep[] = [step("read_file", "completed", 0)];
  const s = composeActivityStream(metaSteps, steps, [], 3);
  const seg = s.segments[0];
  // Ogni evento non-switch ha provider+model del segmento.
  for (const ev of seg.events) {
    if (ev.type === "switch") continue;
    assert.equal(ev.provider, "google", `provider stampato su ${ev.type}`);
    assert.equal(ev.model, "gemini-2.5-pro", `model stampato su ${ev.type}`);
  }
});

test("provenance: il TOOL prende il model EFFETTIVO della trace della sua iterazione", () => {
  beforeEach();
  const runId = "run-1";
  const metaSteps: MetaStepEntry[] = [
    meta("routing", { intent: "x", provider: "anthropic", model: "claude-haiku" }),
  ];
  const steps: AgentStep[] = [step("run", "failed", 3, { toolResult: "e" })];
  // La trace dell'iterazione 3 dichiara un model piu' potente (upscale).
  const traces = [trace(runId, 3, "anthropic", "claude-sonnet")];
  const s = composeActivityStream(metaSteps, steps, traces, 3);
  const tool = s.segments
    .flatMap((seg) => seg.events)
    .find((e): e is ToolEvent => e.type === "tool");
  assert.ok(tool);
  assert.equal(tool.provider, "anthropic");
  // Model effettivo della trace, non quello del segmento.
  assert.equal(tool.model, "claude-sonnet");
});

test("provenance: il folded_tools eredita provider/model del segmento", () => {
  beforeEach();
  const metaSteps: MetaStepEntry[] = [
    meta("routing", { intent: "x", provider: "openai", model: "gpt-4o" }),
  ];
  const steps: AgentStep[] = [
    step("read_file", "completed", 0),
    step("read_file", "completed", 1),
    step("read_file", "completed", 2),
    step("read_file", "completed", 3),
  ];
  const s = composeActivityStream(metaSteps, steps, [], 3);
  const folded = s.segments.flatMap((seg) => seg.events).find((e) => e.type === "folded_tools");
  assert.ok(folded && folded.type === "folded_tools");
  assert.equal(folded.provider, "openai");
  assert.equal(folded.model, "gpt-4o");
  // I tool conservati mantengono la propria provenance stampata.
  assert.ok(folded.tools.every((t) => t.provider === "openai"));
});

test("folded: i tool conservati mantengono input/result (espandibili singolarmente)", () => {
  beforeEach();
  const steps: AgentStep[] = [
    step("read_file", "completed", 0, { toolInput: { path: "a.ts" }, toolResult: "contenuto a" }),
    step("read_file", "completed", 1, { toolInput: { path: "b.ts" }, toolResult: "contenuto b" }),
    step("read_file", "completed", 2, { toolInput: { path: "c.ts" }, toolResult: "contenuto c" }),
  ];
  const s = composeActivityStream([], steps, [], 3);
  const folded = s.segments[0].events.find((e) => e.type === "folded_tools");
  assert.ok(folded && folded.type === "folded_tools");
  assert.equal(folded.tools.length, 3);
  assert.deepEqual(folded.tools[0].input, { path: "a.ts" });
  assert.equal(folded.tools[2].result, "contenuto c");
});

// ── Cap live (capStreamToRecent) ────────────────────────────────────────────

test("cap: sotto soglia -> stream invariato, hiddenCount 0", () => {
  beforeEach();
  const steps: AgentStep[] = [
    step("read_file", "failed", 0), // failed rompe il folding -> resta tool singolo
    step("edit_file", "failed", 1),
  ];
  const s = composeActivityStream([], steps, [], 3);
  const capped = capStreamToRecent(s, 5);
  assert.equal(capped.hiddenCount, 0);
  assert.equal(capped.totalEvents, 2);
  assert.strictEqual(capped.stream, s, "stream identico quando sotto soglia");
});

test("cap: sopra soglia -> tiene ultimi K, hiddenCount corretto, piu' recente in fondo", () => {
  beforeEach();
  // 5 tool tutti FALLITI (evita il folding) -> 5 eventi non-switch.
  const steps: AgentStep[] = [
    step("run", "failed", 0, { toolResult: "err0" }),
    step("run", "failed", 1, { toolResult: "err1" }),
    step("run", "failed", 2, { toolResult: "err2" }),
    step("run", "failed", 3, { toolResult: "err3" }),
    step("run", "failed", 4, { toolResult: "err4" }),
  ];
  const s = composeActivityStream([], steps, [], 3);
  const capped = capStreamToRecent(s, 2);
  assert.equal(capped.totalEvents, 5);
  assert.equal(capped.hiddenCount, 3, "5 - 2 = 3 nascosti");
  const kept = capped.stream.segments.flatMap((seg) =>
    seg.events.filter((e): e is ToolEvent => e.type === "tool"),
  );
  assert.equal(kept.length, 2);
  // L'evento piu' recente (iter 4) e' l'ultimo tenuto.
  assert.equal(kept[kept.length - 1].iteration, 4);
  assert.equal(kept[0].iteration, 3);
});

test("cap: le bande switch restano SEMPRE visibili anche se cappate", () => {
  beforeEach();
  const metaSteps: MetaStepEntry[] = [
    meta("executor_call", { iteration: 0, provider: "google", model: "gemini" }),
    // molti tool sul primo provider (falliti per evitare folding)
    ...([] as MetaStepEntry[]),
    meta("escalation", {
      from_provider: "google",
      to_provider: "anthropic",
      to_model: "claude",
      reason: "loop",
    }),
    meta("executor_call", { iteration: 10, provider: "anthropic", model: "claude" }),
  ];
  const steps: AgentStep[] = [
    step("run", "failed", 0, { toolResult: "e" }),
    step("run", "failed", 1, { toolResult: "e" }),
    step("run", "failed", 2, { toolResult: "e" }),
    step("run", "failed", 10, { toolResult: "e" }),
  ];
  const s = composeActivityStream(metaSteps, steps, [], 3);
  // cap molto stretto: 1 evento. La banda switch (segmento anthropic) resta.
  const capped = capStreamToRecent(s, 1);
  const switchSeg = capped.stream.segments.find((seg) => seg.openedBySwitch);
  assert.ok(switchSeg, "il segmento aperto da switch e' preservato");
  assert.ok(switchSeg.switch, "la banda switch e' preservata");
  assert.equal(switchSeg.switch?.toProvider, "anthropic");
});
