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
  figureVerdictDisplay,
  foldConsecutiveOkTools,
  aggregateTokensByProvider,
  providerCostBreakdown,
  tracesForRun,
  capStreamToRecent,
  raggruppaBlocchiNastro,
  activityLocalAnchorId,
  segmentAnchorId,
  type ActivityEvent,
  type ToolEvent,
  type ReviewGateEvent,
} from "./activity-stream.ts";
import { costFromCatalog, findCatalogEntry } from "../model-catalog.ts";
import type { MetaStepEntry } from "./types.ts";
import type { AgentStep, AITraceEvent } from "../api/agent.ts";
import type { ModelCatalogEntry } from "../api/models.ts";

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

/** Riga di listino: le sole tariffe variano, il resto e' il contratto reale di
 *  `ModelCatalogEntry` (niente cast: un fixture che aggira il tipo smette di
 *  misurare il codice vero appena il contratto cambia). */
function listino(
  provider: string,
  model: string,
  inputPerMln: number,
  outputPerMln: number,
): ModelCatalogEntry {
  return {
    provider,
    model,
    displayName: model,
    inputCostPerMillionTokens: inputPerMln,
    outputCostPerMillionTokens: outputPerMln,
    cacheReadCostPerMillionTokens: null,
    currency: "USD",
    performanceTier: null,
    tierSource: null,
    agenticIndex: null,
    qualificationState: null,
    speedTier: "medium",
    capabilities: [],
    contextWindow: 128_000,
    supportsToolUse: true,
    batchDiscountPct: 0,
    isFeatured: false,
    isEnabled: true,
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

test("escalation con causa strutturata la porta nello SwitchEvent", () => {
  beforeEach();
  // Il motivo onesto dello switch (regola M): cause dal vocabolario chiuso
  // ProviderFailureCause del backend; il renderer lo mappa in etichetta umana
  // (un 4xx del provider non va raccontato come cooldown).
  const metaSteps: MetaStepEntry[] = [
    meta("executor_call", { iteration: 1, provider: "deepseek", model: "deepseek-chat" }),
    meta("escalation", {
      from_provider: "deepseek",
      to_provider: "google",
      to_model: "gemini-2.5-flash",
      reason: "provider_failover",
      cause: "client_error",
      cooldown: false,
    }),
  ];
  const stream = composeActivityStream(metaSteps, [], [], 3);
  const sw = stream.segments[1]?.switch;
  assert.ok(sw, "segmento aperto dallo switch");
  assert.equal(sw?.cause, "client_error");
  // cooldown=false (bool) NON deve diventare una stringa-causa spuria.
  assert.equal(sw?.cooldown, undefined);
});

test("consiglio competenze usa meta-step strutturato dedicato", () => {
  beforeEach();
  const metaSteps: MetaStepEntry[] = [
    meta(
      "council_of_competencies",
      {
        product_name: "Consiglio delle Competenze",
        activation_source: "agentic_deterministic_complexity_scope_analysis",
        signal: "council_synthesis_present",
        activated: true,
      },
      "Consiglio delle Competenze attivo",
    ),
  ];

  const stream = composeActivityStream(metaSteps, [], [], 3);
  const event = stream.segments
    .flatMap((seg) => seg.events)
    .find((e): e is Extract<ActivityEvent, { type: "council_of_competencies" }> =>
      e.type === "council_of_competencies",
    );

  assert.ok(event, "evento Consiglio delle Competenze atteso");
  assert.equal(event.productName, "Consiglio delle Competenze");
  assert.equal(event.activationSource, "agentic_deterministic_complexity_scope_analysis");
});

test("consiglio in segmento unknown eredita provider dal run principale", () => {
  beforeEach();
  const metaSteps: MetaStepEntry[] = [
    meta(
      "council_of_competencies",
      {
        product_name: "Consiglio delle Competenze",
        signal: "council_convening",
        phase: "convening",
        figure_count: 3,
        completed_count: 0,
      },
      "Consiglio in corso (0/3)",
    ),
    meta(
      "fallback",
      {
        from_provider: "openai",
        to_provider: "deepseek",
        to_model: "deepseek-v4-flash",
        reason: "cooldown",
        cause: "cooldown",
      },
      "Fallback su deepseek/deepseek-v4-flash",
    ),
  ];
  const stream = composeActivityStream(metaSteps, [], [], 3);
  const council = stream.segments[0]?.events.find(
    (e): e is Extract<ActivityEvent, { type: "council_of_competencies" }> =>
      e.type === "council_of_competencies",
  );
  assert.ok(council, "evento consiglio atteso");
  assert.equal(council.provider, "deepseek");
  assert.equal(council.model, "deepseek-v4-flash");
});

test("consiglio in corso espone figure_tasks e aggiorna lo stesso evento", () => {
  beforeEach();
  const metaSteps: MetaStepEntry[] = [
    meta(
      "council_of_competencies",
      {
        product_name: "Consiglio delle Competenze",
        signal: "council_convening",
        phase: "convening",
        figure_count: 3,
        completed_count: 0,
        figure_tasks: [
          { kind: "security_engineer", status: "running" },
          { kind: "software_architect", status: "running" },
          { kind: "project_manager", status: "running" },
        ],
      },
      "Consiglio in corso (0/3)",
    ),
    meta(
      "council_of_competencies",
      {
        product_name: "Consiglio delle Competenze",
        signal: "council_convening",
        phase: "convening",
        figure_count: 3,
        completed_count: 2,
        figure_tasks: [
          { kind: "security_engineer", status: "done" },
          { kind: "software_architect", status: "done" },
          { kind: "project_manager", status: "running" },
        ],
      },
      "Consiglio in corso (2/3)",
    ),
    meta(
      "council_of_competencies",
      {
        product_name: "Consiglio delle Competenze",
        signal: "council_synthesis_present",
        activated: true,
        degraded: false,
        figure_count: 3,
      },
      "Consiglio delle Competenze attivo",
    ),
  ];

  const stream = composeActivityStream(metaSteps, [], [], 3);
  const events = stream.segments.flatMap((seg) => seg.events).filter(
    (e): e is Extract<ActivityEvent, { type: "council_of_competencies" }> =>
      e.type === "council_of_competencies",
  );

  assert.equal(events.length, 1, "convening progress deve essere upsertato");
  assert.equal(events[0]?.phase, "complete");
  assert.equal(events[0]?.degraded, false);
});

test("consiglio competenze degradato espone figure_reports strutturati", () => {
  beforeEach();
  const metaSteps: MetaStepEntry[] = [
    meta(
      "council_of_competencies",
      {
        product_name: "Consiglio delle Competenze",
        activation_source: "agentic_deterministic_complexity_scope_analysis",
        signal: "council_degraded",
        activated: false,
        degraded: true,
        degradation_reason: "synthesis_unavailable",
        degradation_detail:
          "Nessuna figura ha prodotto un parere advisory valido.",
        figure_count: 2,
        figure_reports: [
          {
            kind: "security_engineer",
            status: "prepare_failed",
            detail_code: "depth_exceeded",
            detail_message: "depth 3 > max 2",
          },
          {
            kind: "software_architect",
            status: "completed_no_advisory",
            detail_code: "no_advisory",
            detail_message: "Sub-run completato senza chiamare advisory_verdict",
          },
        ],
      },
      "Consiglio delle Competenze degradato (2 figure)",
    ),
  ];

  const stream = composeActivityStream(metaSteps, [], [], 3);
  const event = stream.segments
    .flatMap((seg) => seg.events)
    .find((e): e is Extract<ActivityEvent, { type: "council_of_competencies" }> =>
      e.type === "council_of_competencies",
    );

  assert.ok(event, "evento Consiglio degradato atteso");
  assert.equal(event.degraded, true);
  assert.equal(event.figureReports?.length, 2);
  assert.equal(event.figureReports?.[0]?.detail_code, "depth_exceeded");
});

test("consiglio competenze propaga il parere advisory completo di ogni figura", () => {
  beforeEach();
  const metaSteps: MetaStepEntry[] = [
    meta(
      "council_of_competencies",
      {
        product_name: "Consiglio delle Competenze",
        signal: "council_synthesis_present",
        activated: true,
        figure_count: 1,
        figure_reports: [
          {
            kind: "security_engineer",
            status: "advisory_ok",
            detail_code: "advisory_ok",
            detail_message: "Parere advisory valido",
            advisory_verdict: "proceed_with_changes",
            advisory: {
              verdict: "proceed_with_changes",
              requirements: ["Cifrare i token a riposo"],
              risks: [{ severity: "alta", description: "2FA senza rate limit" }],
              recommendations: ["Aggiungere audit log"],
            },
          },
        ],
      },
      "Consiglio delle Competenze",
    ),
  ];

  const stream = composeActivityStream(metaSteps, [], [], 3);
  const event = stream.segments
    .flatMap((seg) => seg.events)
    .find((e): e is Extract<ActivityEvent, { type: "council_of_competencies" }> =>
      e.type === "council_of_competencies",
    );

  assert.ok(event, "evento Consiglio atteso");
  const report = event.figureReports?.[0];
  assert.ok(report, "report figura atteso");
  assert.equal(report.advisory_verdict, "proceed_with_changes");
  assert.ok(report.advisory, "parere advisory strutturato atteso");
  assert.equal(report.advisory.verdict, "proceed_with_changes");
  assert.deepEqual(report.advisory.requirements, ["Cifrare i token a riposo"]);
  assert.equal(report.advisory.risks?.length, 1);
  assert.equal(report.advisory.risks?.[0]?.severity, "alta");
  assert.deepEqual(report.advisory.recommendations, ["Aggiungere audit log"]);
});

test("etichetta figura distingue veto e cause tecniche, mai un opaco n/d", () => {
  // Regola O: si parte dai figure_reports (shape del backend) e si attraversa il
  // produttore reale (composeActivityStream -> readFigureReports), poi il punto
  // unico figureVerdictDisplay. Scenario del run reale 2026-07-18 04:42: un
  // block con evidenza (NON declassato) + quattro astensioni tecniche distinte.
  beforeEach();
  const metaSteps: MetaStepEntry[] = [
    meta(
      "council_of_competencies",
      {
        product_name: "Consiglio delle Competenze",
        signal: "council_synthesis_present",
        activated: true,
        figure_count: 6,
        figure_reports: [
          {
            kind: "provider_analyst",
            status: "advisory_ok",
            detail_code: "advisory_ok",
            detail_message: "Parere advisory valido",
            advisory_verdict: "block",
            advisory: {
              verdict: "block",
              risks: [{ severity: "alta", description: "manca request_port" }],
            },
          },
          {
            kind: "project_manager",
            status: "run_timeout",
            detail_code: "run_timeout",
            detail_message: "Sub-agent in timeout",
          },
          {
            kind: "sysadmin",
            status: "run_failed",
            detail_code: "billing_error",
            detail_message: "Sub-run terminato senza esito positivo",
          },
          {
            kind: "software_architect",
            status: "completed_no_advisory",
            detail_code: "no_advisory",
            detail_message: "Sub-run completato senza chiamare advisory_verdict",
          },
          {
            kind: "security_engineer",
            status: "invalid_advisory",
            detail_code: "invalid_advisory",
            detail_message: "Parere advisory presente ma verdetto non valido",
            advisory_verdict: "reject",
            advisory: { verdict: "reject" },
          },
        ],
      },
      "Consiglio delle Competenze",
    ),
  ];

  const stream = composeActivityStream(metaSteps, [], [], 3);
  const event = stream.segments
    .flatMap((seg) => seg.events)
    .find((e): e is Extract<ActivityEvent, { type: "council_of_competencies" }> =>
      e.type === "council_of_competencies",
    );
  assert.ok(event, "evento Consiglio atteso");
  const reports = event.figureReports;
  assert.equal(reports?.length, 5);

  const byKind = new Map(reports!.map((r) => [r.kind, figureVerdictDisplay(r)]));

  // Il veto con evidenza NON e' un'astensione: e' "blocca", tono block.
  assert.deepEqual(byKind.get("provider_analyst"), { tone: "block", label: "blocca" });
  // Le cause tecniche hanno etichette PROPRIE e distinte, non un unico "n/d".
  assert.deepEqual(byKind.get("project_manager"), { tone: "technical", label: "tempo scaduto" });
  assert.deepEqual(byKind.get("sysadmin"), { tone: "technical", label: "errore" });
  assert.deepEqual(byKind.get("software_architect"), {
    tone: "technical",
    label: "nessun parere",
  });
  assert.deepEqual(byKind.get("security_engineer"), {
    tone: "invalid",
    label: "parere non valido",
  });

  // Nessuna figura di questo scenario cade sull'opaco "n/d": il difetto era
  // proprio questo collasso. Il veto in particolare non deve mai sembrare muto.
  for (const d of byKind.values()) {
    assert.notEqual(d.label, "n/d");
  }
});

test("consiglio competenze degradato espone segnale strutturato", () => {
  beforeEach();
  const metaSteps: MetaStepEntry[] = [
    meta(
      "council_of_competencies",
      {
        product_name: "Consiglio delle Competenze",
        activation_source: "agentic_deterministic_complexity_scope_analysis",
        signal: "council_degraded",
        activated: false,
        degraded: true,
        degradation_reason: "subagents_disabled",
        degradation_detail:
          "Sub-agents disabilitati (orchestrator.subagents_enabled=false): impossibile convocare le figure.",
        figure_count: 5,
      },
      "Consiglio delle Competenze degradato (5 figure)",
    ),
  ];

  const stream = composeActivityStream(metaSteps, [], [], 3);
  const event = stream.segments
    .flatMap((seg) => seg.events)
    .find((e): e is Extract<ActivityEvent, { type: "council_of_competencies" }> =>
      e.type === "council_of_competencies",
    );

  assert.ok(event, "evento Consiglio degradato atteso");
  assert.equal(event.degraded, true);
  assert.match(
    event.degradationReason ?? "",
    /subagents_enabled=false/,
  );
});

test("multi-provider panel usa meta-step strutturato dedicato", () => {
  beforeEach();
  const metaSteps: MetaStepEntry[] = [
    meta(
      "multi_provider_panel",
      {
        product_name: "Multi-provider advisory",
        activation_source: "agentic_deterministic_multi_provider_panel",
        signal: "multi_provider_synthesis_present",
        activated: true,
        degraded: false,
        provider_count: 3,
        panel_providers: [
          { provider: "deepseek", model: "deepseek-v4-flash" },
          { provider: "google", model: "gemini-2.5-flash" },
          { provider: "mistral", model: "mistral-small-latest" },
        ],
      },
      "Panel multi-provider attivo (3)",
    ),
  ];

  const stream = composeActivityStream(metaSteps, [], [], 3);
  const event = stream.segments
    .flatMap((seg) => seg.events)
    .find((e): e is Extract<ActivityEvent, { type: "multi_provider_panel" }> =>
      e.type === "multi_provider_panel",
    );

  assert.ok(event, "evento multi-provider atteso");
  assert.equal(event.productName, "Multi-provider advisory");
  assert.equal(event.providerCount, 3);
  assert.equal(event.panelProviders?.length, 3);
  assert.equal(event.panelProviders?.[0]?.provider, "deepseek");
  assert.equal(event.degraded, false);
});

test("multi-provider panel degradato espone segnale strutturato", () => {
  beforeEach();
  const metaSteps: MetaStepEntry[] = [
    meta(
      "multi_provider_panel",
      {
        product_name: "Multi-provider advisory",
        signal: "multi_provider_degraded",
        activated: false,
        degraded: true,
        degradation_reason: "insufficient_provider_diversity",
        provider_count_got: 1,
        provider_count_min: 2,
      },
      "Panel multi-provider degradato (1/2)",
    ),
  ];

  const stream = composeActivityStream(metaSteps, [], [], 3);
  const event = stream.segments
    .flatMap((seg) => seg.events)
    .find((e): e is Extract<ActivityEvent, { type: "multi_provider_panel" }> =>
      e.type === "multi_provider_panel",
    );

  assert.ok(event);
  assert.equal(event.degraded, true);
  assert.equal(event.degradationReason, "insufficient_provider_diversity");
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

test("tracesForRun include le trace dei sub-run, dichiarate dal parentRunId del wire", () => {
  beforeEach();
  // Il subagente e' un run a se': le sue trace stanno sotto il PROPRIO run_id.
  // Filtrando per il solo run del padre sparivano dal footer costo-per-provider,
  // insieme ai provider usati SOLO dal figlio. La parentela e' quella che il
  // backend annota sulla traccia leggendola dal DB (run_lineage.rs), non quella
  // dedotta dai meta-step di narrazione (che il review panel non emette).
  const traces = [
    trace("padre", 1, "mistral", "mistral-large-2512", { inputTokens: 100, outputTokens: 20 }),
    trace("sub-1", 1, "groq", "gpt-oss-120b", {
      inputTokens: 300,
      outputTokens: 50,
      parentRunId: "padre",
    }),
    trace("altro-run", 1, "openai", "gpt", { inputTokens: 999, outputTokens: 999 }),
  ];

  const conFigli = tracesForRun(traces, "padre");
  assert.equal(conFigli.length, 2);
  // Il run NON correlato resta fuori: si includono solo i sub-run dichiarati.
  assert.equal(conFigli.some((t) => t.runId === "altro-run"), false);
  // Il figlio, guardato come run a se', porta solo le proprie trace.
  assert.equal(tracesForRun(traces, "sub-1").length, 1);

  const buckets = aggregateTokensByProvider(conFigli);
  const groq = buckets.find((b) => b.provider === "groq");
  assert.equal(groq?.inputTokens, 300, "i token del subagente sono contabilizzati");
  assert.equal(groq?.outputTokens, 50);
});

// ── Il difetto misurato il 26/07/2026 sul progetto e2e-todo ────────────────
// La barra dichiarava la ripartizione per provider di un run e mostrava una sola
// voce, "deepseek": i 4 cicli di review su openrouter/z-ai/glm-4.7-flash (21
// iterazioni complessive, costo registrato in nexus_subagent_runs) non c'erano.
// Non un'approssimazione: un provider intero omesso.
//
// Il test attraversa la stessa composizione che usa il footer
// (`providerCostBreakdown`, con il prezzo calcolato dal punto unico
// `costFromCatalog`) e parte dalle trace nella forma in cui arrivano dal wire.
// MUTAZIONE: se `tracesForRun` torna a ignorare `parentRunId`, la voce openrouter
// sparisce e il totale cala esattamente del costo del revisore.
test("la ripartizione elenca il provider del revisore e il totale ne contiene il costo", () => {
  beforeEach();
  const catalogo: ModelCatalogEntry[] = [
    listino("deepseek", "deepseek-v4-flash", 0.28, 0.42),
    listino("openrouter", "z-ai/glm-4.7-flash", 0.1, 0.3),
  ];
  const prezzo = (b: { provider: string; model: string; inputTokens: number; outputTokens: number }) =>
    costFromCatalog(findCatalogEntry(catalogo, b.provider, b.model), b.inputTokens, b.outputTokens) ?? 0;

  // Token del ciclo di review piu' lungo misurato (12 iterazioni).
  const REV_IN = 30_991;
  const REV_OUT = 3_544;
  const traces = [
    trace("padre", 1, "deepseek", "deepseek-v4-flash", { inputTokens: 200_000, outputTokens: 9_000 }),
    trace("review-1", 1, "openrouter", "z-ai/glm-4.7-flash", {
      inputTokens: REV_IN,
      outputTokens: REV_OUT,
      parentRunId: "padre",
    }),
  ];

  const ripartizione = providerCostBreakdown(tracesForRun(traces, "padre"), prezzo);
  const providers = ripartizione.voci.map((v) => v.provider).sort();
  assert.deepEqual(providers, ["deepseek", "openrouter"], "il revisore ha una sua voce");

  // I dollari si confrontano a meno dell'ultimo bit di virgola mobile: sommare
  // in ordine diverso cambia il risultato oltre la 15a cifra, e un'uguaglianza
  // esatta renderebbe il test fragile su una differenza che non esiste.
  const quasiUguale = (a: number, b: number, msg: string) =>
    assert.ok(Math.abs(a - b) < 1e-12, `${msg}: ${a} != ${b}`);

  const costoRevisore = (REV_IN * 0.1 + REV_OUT * 0.3) / 1_000_000;
  const voceRevisore = ripartizione.voci.find((v) => v.provider === "openrouter");
  quasiUguale(voceRevisore?.costUsd ?? 0, costoRevisore, "costo della voce revisore");
  assert.equal(voceRevisore?.inputTokens, REV_IN);

  // Il totale CONTIENE il contributo del revisore: la differenza con la
  // ripartizione del solo run padre e' esattamente il suo costo (e i suoi token).
  const soloPadre = providerCostBreakdown(
    traces.filter((t) => t.runId === "padre"),
    prezzo,
  );
  quasiUguale(
    ripartizione.totalCostUsd - soloPadre.totalCostUsd,
    costoRevisore,
    "quota del revisore nel totale",
  );
  assert.equal(ripartizione.totalTokens - soloPadre.totalTokens, REV_IN + REV_OUT);
});

test("tracesForRun risale la catena: anche i nipoti appartengono al run", () => {
  beforeEach();
  // Un sub-run che ne convoca un altro: il nipote e' elencato PRIMA del padre
  // intermedio, cosi' un solo passaggio di filtro lo perderebbe.
  const traces = [
    trace("nipote", 1, "openrouter", "glm", { inputTokens: 10, outputTokens: 5, parentRunId: "figlio" }),
    trace("figlio", 1, "groq", "oss", { inputTokens: 20, outputTokens: 5, parentRunId: "padre" }),
    trace("padre", 1, "deepseek", "v4", { inputTokens: 30, outputTokens: 5 }),
  ];
  const ids = tracesForRun(traces, "padre").map((t) => t.runId).sort();
  assert.deepEqual(ids, ["figlio", "nipote", "padre"]);
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

// ── ReviewGate nel nastro (kind=review_gate, nodo review_gate.rs) ───────────

test("review_gate: il rimando in correzione compare nel nastro col titolo del backend", () => {
  beforeEach();
  // Payload del ramo boccia() non-definitivo di review_gate.rs (phase=failed).
  const metaSteps: MetaStepEntry[] = [
    meta(
      "review_gate",
      { cycle: 1, max_cycles: 1, findings: 3, phase: "failed", verdict: "needs_changes" },
      "Review NON superata: rimando in correzione (1/1)",
    ),
  ];
  const s = composeActivityStream(metaSteps, [], [], 3);
  const ev = s.segments
    .flatMap((seg) => seg.events)
    .find((e): e is ReviewGateEvent => e.type === "review_gate");
  assert.ok(ev, "l'evento review_gate NON deve essere scartato dal parser");
  assert.equal(ev.title, "Review NON superata: rimando in correzione (1/1)");
  assert.equal(ev.phase, "failed");
  assert.equal(ev.verdict, "needs_changes");
  assert.equal(ev.cycle, 1);
  assert.equal(ev.maxCycles, 1);
});

test("review_gate: la chiusura approvata porta il verdetto pass", () => {
  beforeEach();
  // Payload del ramo close_not_rejected() (phase=closed).
  const metaSteps: MetaStepEntry[] = [
    meta(
      "review_gate",
      { cycle: 2, phase: "closed", valid: 2, total: 2, verdict: "pass" },
      "Review adversariale: pass (2/2 voti validi)",
    ),
  ];
  const s = composeActivityStream(metaSteps, [], [], 3);
  const ev = s.segments
    .flatMap((seg) => seg.events)
    .find((e): e is ReviewGateEvent => e.type === "review_gate");
  assert.ok(ev);
  assert.equal(ev.verdict, "pass");
  assert.equal(ev.title, "Review adversariale: pass (2/2 voti validi)");
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

test("cap: un provider con soli eventi cappati resta visibile col conteggio, non sparisce", () => {
  beforeEach();
  // deepseek fa 2 step, poi switch a mistral che ne fa 3: il cap stretto nasconde
  // TUTTI gli step di deepseek. Reclamo utente: "non si vedono piu' gli step di
  // deepseek, dopo il refresh ricompaiono". deepseek NON deve sparire dalla vista
  // live: resta col conteggio dei passi compressi.
  // Ordine di creazione INTERLACCIATO: l'associazione step->segmento e' per
  // createdAt, quindi gli step di deepseek vanno creati tra il suo executor_call
  // e l'escalation, altrimenti finirebbero tutti nell'ultimo segmento.
  const m0 = meta("executor_call", { iteration: 0, provider: "deepseek", model: "v4" });
  const sd0 = step("run", "failed", 0, { toolResult: "e" });
  const sd1 = step("run", "failed", 1, { toolResult: "e" });
  const mEsc = meta("escalation", {
    from_provider: "deepseek",
    to_provider: "mistral",
    to_model: "small",
    reason: "loop",
  });
  const mM = meta("executor_call", { iteration: 10, provider: "mistral", model: "small" });
  const sm0 = step("run", "failed", 10, { toolResult: "e" });
  const sm1 = step("run", "failed", 11, { toolResult: "e" });
  const sm2 = step("run", "failed", 12, { toolResult: "e" });
  const s = composeActivityStream([m0, mEsc, mM], [sd0, sd1, sm0, sm1, sm2], [], 3);
  const capped = capStreamToRecent(s, 2); // tiene solo gli ultimi 2 (mistral)
  const deepseekSeg = capped.stream.segments.find((seg) => seg.provider === "deepseek");
  assert.ok(deepseekSeg, "il segmento deepseek NON deve sparire dalla vista live");
  assert.equal(deepseekSeg.cappedCount, 2, "mostra i 2 passi compressi di deepseek");
  assert.equal(
    deepseekSeg.events.filter((e) => e.type !== "switch").length,
    0,
    "gli eventi di deepseek sono cappati (dettaglio nello storico)",
  );
});

// ── 8. Narrazione sub-agente (subagent_started/progress/completed) ──────────

test("i meta-step subagent_* diventano eventi 'subagent' con fase strutturata", () => {
  beforeEach();
  const subRunId = "8f1e0b2a-0000-0000-0000-000000000001";
  const metaSteps: MetaStepEntry[] = [
    meta("executor_call", { iteration: 0, provider: "google", model: "gemini" }),
    meta(
      "subagent_started",
      { subagent_run_id: subRunId, subagent_kind: "coder", task: "fix bug" },
      "Subagente coder avviato — fix bug",
    ),
    meta(
      "subagent_progress",
      { phase: "tool", tool: "edit_file", target: "src/a.rs", is_error: false, subagent_run_id: subRunId, subagent_kind: "coder" },
      "subagente coder: tool edit_file — src/a.rs",
    ),
    meta(
      "subagent_completed",
      { status: "completed", summary: "fatto", iterations: 7, cost_usd: 0.02, subagent_run_id: subRunId, subagent_kind: "coder" },
      "Subagente coder completato (7 iterazioni)",
    ),
  ];
  const s = composeActivityStream(metaSteps, [], [], 3);
  const events = s.segments.flatMap((seg) => seg.events);
  const subs = events.filter((e): e is Extract<ActivityEvent, { type: "subagent" }> => e.type === "subagent");
  assert.equal(subs.length, 3, "avvio + progresso + chiusura");
  assert.equal(subs[0].phase, "started");
  assert.equal(subs[0].subagentKind, "coder");
  assert.equal(subs[1].phase, "tool");
  assert.equal(subs[1].tool, "edit_file");
  assert.equal(subs[1].isError, false);
  assert.equal(subs[2].phase, "completed");
  assert.equal(subs[2].summary, "fatto");
  assert.equal(subs[2].iterations, 7);
  // Correlazione: tutti gli eventi portano lo stesso subagentRunId.
  assert.ok(subs.every((e) => e.subagentRunId === subRunId));
});

test("gli heartbeat 'working' consecutivi dello stesso sub-run collassano nell'ultimo", () => {
  beforeEach();
  const subRunId = "8f1e0b2a-0000-0000-0000-000000000002";
  const hb = (elapsed: number) =>
    meta(
      "subagent_progress",
      { phase: "working", elapsed_s: elapsed, subagent_run_id: subRunId, subagent_kind: "tester" },
      `Subagente tester: al lavoro da ${elapsed}s`,
    );
  const metaSteps: MetaStepEntry[] = [
    meta("subagent_started", { subagent_run_id: subRunId, subagent_kind: "tester" }, "Subagente tester avviato"),
    hb(20),
    hb(40),
    hb(60),
    meta("subagent_completed", { status: "completed", subagent_run_id: subRunId }, "Subagente tester completato"),
  ];
  const s = composeActivityStream(metaSteps, [], [], 3);
  const events = s.segments.flatMap((seg) => seg.events);
  const subs = events.filter((e): e is Extract<ActivityEvent, { type: "subagent" }> => e.type === "subagent");
  assert.equal(subs.length, 3, "avvio + UN solo heartbeat (l'ultimo) + chiusura");
  assert.equal(subs[1].phase, "working");
  assert.equal(subs[1].elapsedS, 60, "resta l'heartbeat piu' recente");
  // L'errore strutturato del subagent_failed marca l'evento.
  const failed = composeActivityStream(
    [meta("subagent_failed", { status: "timeout", error: "[Sub-agent timeout]", subagent_run_id: subRunId }, "Subagente tester in timeout")],
    [],
    [],
    3,
  );
  const fev = failed.segments.flatMap((seg) => seg.events).find((e) => e.type === "subagent");
  assert.ok(fev && fev.type === "subagent");
  assert.equal(fev.phase, "failed");
  assert.equal(fev.isError, true);
  assert.equal(fev.summary, "[Sub-agent timeout]");
});

test("batch parallelo: gli heartbeat INTERLACCIATI di sub-run diversi restano distinti (uno per run)", () => {
  beforeEach();
  const runA = "aaaaaaaa-0000-0000-0000-000000000001";
  const runB = "bbbbbbbb-0000-0000-0000-000000000002";
  const hb = (run: string, kind: string, elapsed: number) =>
    meta(
      "subagent_progress",
      { phase: "working", elapsed_s: elapsed, subagent_run_id: run, subagent_kind: kind },
      `Subagente ${kind}: al lavoro da ${elapsed}s`,
    );
  // Heartbeat interlacciati (A e B alternati): il collasso deve tenere UN solo
  // working per run, non confondere i due (il vecchio confronto col solo `last`
  // non li avrebbe compressi -> 4 righe invece di 2).
  const metaSteps: MetaStepEntry[] = [
    meta("subagent_started", { subagent_run_id: runA, subagent_kind: "coder" }, "Subagente coder avviato"),
    meta("subagent_started", { subagent_run_id: runB, subagent_kind: "tester" }, "Subagente tester avviato"),
    hb(runA, "coder", 20),
    hb(runB, "tester", 20),
    hb(runA, "coder", 40),
    hb(runB, "tester", 40),
  ];
  const s = composeActivityStream(metaSteps, [], [], 3);
  const subs = s.segments
    .flatMap((seg) => seg.events)
    .filter((e): e is Extract<ActivityEvent, { type: "subagent" }> => e.type === "subagent");
  const workingA = subs.filter((e) => e.phase === "working" && e.subagentRunId === runA);
  const workingB = subs.filter((e) => e.phase === "working" && e.subagentRunId === runB);
  assert.equal(workingA.length, 1, "un solo working per il run A");
  assert.equal(workingB.length, 1, "un solo working per il run B");
  assert.equal(workingA[0].elapsedS, 40, "resta l'heartbeat piu' recente di A");
  assert.equal(workingB[0].elapsedS, 40, "resta l'heartbeat piu' recente di B");
});

// ── 9. Provenienza (provider/model) del sub-agente ──────────────────────────
// Il provider del blocco SUBAGENTE e' quello del FIGLIO (payload del ponte,
// regola M), mai quello del segmento padre. Lo started, emesso prima che il
// routing del figlio scelga il modello, si aggiorna retroattivamente col primo
// progress che porta la provenienza; ignoto vero -> "unknown" (icona '?').

test("la provenienza subagent viene dal payload del figlio, mai dal segmento padre", () => {
  beforeEach();
  const subRunId = "8f1e0b2a-0000-0000-0000-000000000009";
  const metaSteps: MetaStepEntry[] = [
    meta("executor_call", { iteration: 0, provider: "google", model: "gemini-2.5-pro" }),
    meta(
      "subagent_started",
      { subagent_run_id: subRunId, subagent_kind: "coder", task: "fix" },
      "Subagente coder avviato — fix",
    ),
    meta(
      "subagent_progress",
      {
        phase: "tool", tool: "edit_file", is_error: false,
        subagent_run_id: subRunId, subagent_kind: "coder",
        provider: "anthropic", model: "claude-haiku-4-5",
      },
      "subagente coder: tool edit_file",
    ),
    meta(
      "subagent_progress",
      { phase: "working", elapsed_s: 20, subagent_run_id: subRunId, subagent_kind: "coder" },
      "Subagente coder: al lavoro da 20s",
    ),
  ];
  const s = composeActivityStream(metaSteps, [], [], 3);
  const subs = s.segments
    .flatMap((seg) => seg.events)
    .filter((e): e is Extract<ActivityEvent, { type: "subagent" }> => e.type === "subagent");
  assert.equal(subs.length, 3);
  // Lo started si aggiorna RETROATTIVAMENTE col provider del primo progress.
  assert.equal(subs[0].phase, "started");
  assert.equal(subs[0].provider, "anthropic", "started eredita dal primo progress del figlio");
  assert.equal(subs[0].model, "claude-haiku-4-5");
  // Il progress porta la propria provenienza dal payload.
  assert.equal(subs[1].provider, "anthropic");
  // L'heartbeat senza payload eredita l'ultimo provider noto del run (forward).
  assert.equal(subs[2].provider, "anthropic");
  // MAI il provider del segmento padre (google).
  assert.ok(subs.every((e) => e.provider !== "google"), "nessuna attribuzione al padre");
});

test("provenienza subagent davvero ignota degrada a 'unknown' (icona '?'), non al padre", () => {
  beforeEach();
  const subRunId = "8f1e0b2a-0000-0000-0000-00000000000a";
  const metaSteps: MetaStepEntry[] = [
    meta("executor_call", { iteration: 0, provider: "google", model: "gemini-2.5-pro" }),
    meta("subagent_started", { subagent_run_id: subRunId, subagent_kind: "tester" }, "Subagente tester avviato"),
    meta(
      "subagent_progress",
      { phase: "working", elapsed_s: 20, subagent_run_id: subRunId, subagent_kind: "tester" },
      "Subagente tester: al lavoro da 20s",
    ),
  ];
  const s = composeActivityStream(metaSteps, [], [], 3);
  const subs = s.segments
    .flatMap((seg) => seg.events)
    .filter((e): e is Extract<ActivityEvent, { type: "subagent" }> => e.type === "subagent");
  assert.ok(subs.length >= 2);
  for (const ev of subs) {
    assert.equal(ev.provider, "unknown", "ignoto vero -> unknown, mai google");
  }
});

test("il pin allo start (model_purpose) e l'escalation del figlio restano per-evento", () => {
  beforeEach();
  const subRunId = "8f1e0b2a-0000-0000-0000-00000000000b";
  const metaSteps: MetaStepEntry[] = [
    // Pin gia' noto allo start (definition.model_purpose risolto).
    meta(
      "subagent_started",
      { subagent_run_id: subRunId, subagent_kind: "coder", provider: "mistral", model: "devstral" },
      "Subagente coder avviato",
    ),
    // Il figlio poi cambia modello (escalation interna): il progress porta il
    // provider corrente e NON viene sovrascritto dalla propagazione.
    meta(
      "subagent_progress",
      {
        phase: "tool", tool: "run_command", is_error: false,
        subagent_run_id: subRunId, subagent_kind: "coder",
        provider: "anthropic", model: "claude-sonnet",
      },
      "subagente coder: tool run_command",
    ),
    meta(
      "subagent_completed",
      {
        status: "completed", subagent_run_id: subRunId, subagent_kind: "coder",
        provider: "anthropic", model: "claude-sonnet",
      },
      "Subagente coder completato",
    ),
  ];
  const s = composeActivityStream(metaSteps, [], [], 3);
  const subs = s.segments
    .flatMap((seg) => seg.events)
    .filter((e): e is Extract<ActivityEvent, { type: "subagent" }> => e.type === "subagent");
  assert.equal(subs.length, 3);
  assert.equal(subs[0].provider, "mistral", "lo started conserva il pin");
  assert.equal(subs[0].model, "devstral");
  assert.equal(subs[1].provider, "anthropic", "il progress conserva il provider corrente");
  assert.equal(subs[2].provider, "anthropic");
  assert.equal(subs[2].model, "claude-sonnet");
});

// ── Ancoraggio deep-link (regola O) ─────────────────────────────────────────
// L'ancora del deep-link (campanella -> riga del nastro) e' assegnata da
// composeActivityStream: la verifichiamo sul PRODUTTORE reale, mai su uno stream
// costruito a mano (fabbricare un anchorId fossilizzerebbe un valore che il
// produttore non emette). Test di mutazione: se l'assegnazione si rompe, gli
// anchorId diventano undefined e gli assert falliscono.

test("anchoring: composeActivityStream assegna l'ancora canonica a segmenti ed eventi", () => {
  beforeEach();
  const metaSteps: MetaStepEntry[] = [
    meta("routing", { intent: "code_fix", provider: "deepseek", model: "deepseek-chat" }),
    meta("executor_call", { iteration: 1, provider: "deepseek", model: "deepseek-chat" }),
    meta("escalation", {
      from_provider: "deepseek",
      to_provider: "google",
      to_model: "gemini-2.5-flash",
      reason: "x",
      cause: "cooldown",
    }),
    meta("executor_call", { iteration: 3, provider: "google", model: "gemini-2.5-flash" }),
  ];
  const stream = composeActivityStream(metaSteps, [], [], 3);
  assert.ok(stream.segments.length >= 2, "escalation apre un secondo segmento");

  let eventsChecked = 0;
  for (let si = 0; si < stream.segments.length; si++) {
    const seg = stream.segments[si];
    assert.equal(seg.anchorId, segmentAnchorId(si), `ancora del segmento ${si}`);
    for (let ei = 0; ei < seg.events.length; ei++) {
      const ev = seg.events[ei];
      if (ev.type === "switch") continue;
      assert.equal(ev.anchorId, activityLocalAnchorId(si, ei), `ancora evento ${si}/${ei}`);
      eventsChecked += 1;
    }
  }
  // Guardia: il loop deve aver esercitato davvero il ramo evento (altrimenti il
  // test passerebbe a vuoto senza verificare nulla).
  assert.ok(eventsChecked >= 1, "almeno un evento non-switch con ancora verificato");
});

// Raggruppamento dei passi di un sub-agente in blocchi collassabili.
// Il difetto che questi test bloccano: le righe erano una lista piatta, percio'
// i comandi di un sub-agente non si potevano chiudere e restavano tutti a
// schermo anche quando ne partiva un altro.
test("raggruppaBlocchiNastro raccoglie i passi consecutivi dello stesso sub-agente", () => {
  const sub = (id: string, title: string): ActivityEvent =>
    ({ type: "subagent", phase: "tool", subagentRunId: id, title }) as ActivityEvent;
  const blocchi = raggruppaBlocchiNastro([
    sub("run-a", "primo"),
    sub("run-a", "secondo"),
    sub("run-a", "terzo"),
    sub("run-b", "altro sub-agente"),
  ]);
  assert.equal(blocchi.length, 2, "due sub-run distinti, due blocchi");
  assert.equal(blocchi[0].tipo, "gruppo_subagente");
  assert.equal(blocchi[1].tipo, "gruppo_subagente");
  if (blocchi[0].tipo !== "gruppo_subagente" || blocchi[1].tipo !== "gruppo_subagente") return;
  assert.equal(blocchi[0].subagentRunId, "run-a");
  assert.equal(blocchi[0].eventi.length, 3, "i tre passi di run-a stanno in un solo blocco");
  assert.equal(blocchi[1].subagentRunId, "run-b");
  // L'indice e' quello ORIGINALE nel segmento: le key di React restano stabili
  // e il deep-link continua a puntare alla riga giusta.
  assert.equal(blocchi[1].indice, 3);
});

test("raggruppaBlocchiNastro non fonde sub-run interlacciati e salta gli switch", () => {
  const sub = (id: string): ActivityEvent =>
    ({ type: "subagent", phase: "tool", subagentRunId: id, title: id }) as ActivityEvent;
  const blocchi = raggruppaBlocchiNastro([
    sub("run-a"),
    { type: "switch" } as ActivityEvent,
    sub("run-a"),
    sub("run-b"),
    sub("run-a"),
  ]);
  // Lo switch non produce una riga, quindi non spezza il gruppo che attraversa.
  assert.equal(blocchi.length, 3, "a, b, poi di nuovo a: l'ordine reale e' preservato");
  if (blocchi[0].tipo !== "gruppo_subagente") return assert.fail("primo blocco");
  assert.equal(blocchi[0].eventi.length, 2, "lo switch non spezza il gruppo");
  if (blocchi[2].tipo !== "gruppo_subagente") return assert.fail("terzo blocco");
  assert.equal(blocchi[2].subagentRunId, "run-a");
  assert.equal(
    blocchi[2].indice,
    4,
    "run-a che ritorna dopo run-b apre un blocco NUOVO: fonderli mentirebbe sull'ordine",
  );
});

test("raggruppaBlocchiNastro lascia riga singola cio' che non e' sub-agente", () => {
  const blocchi = raggruppaBlocchiNastro([
    { type: "tool", title: "un tool" } as ActivityEvent,
    { type: "subagent", phase: "tool", title: "senza id" } as ActivityEvent,
  ]);
  assert.equal(blocchi.length, 2);
  assert.equal(blocchi[0].tipo, "riga");
  // Un evento subagente SENZA id non e' raggruppabile: non si sa a chi appartiene.
  assert.equal(blocchi[1].tipo, "riga");
});

// Il blocco dei passi compressi nasceva senza provenienza: il suo tooltip
// nominava un provider senza dire su quale modello i passi fossero girati.
test("il blocco compresso eredita provider e modello dai passi che contiene", () => {
  const tool = (i: number): ActivityEvent =>
    ({
      type: "tool",
      outcome: "ok",
      iteration: i,
      title: `passo ${i}`,
      provider: "mistral",
      model: "magistral-small-latest",
    }) as ActivityEvent;
  const out = foldConsecutiveOkTools([tool(1), tool(2), tool(3)], 3);
  assert.equal(out.length, 1, "tre tool ok consecutivi si comprimono in un blocco");
  const blocco = out[0];
  assert.equal(blocco.type, "folded_tools");
  assert.equal(blocco.provider, "mistral");
  assert.equal(
    blocco.model,
    "magistral-small-latest",
    "senza il modello il tooltip del blocco resta monco: nomina il provider ma non su cosa e' girato",
  );
});
