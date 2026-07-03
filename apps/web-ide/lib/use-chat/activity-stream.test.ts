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
