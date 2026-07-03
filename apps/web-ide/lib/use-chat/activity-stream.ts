// Punto unico di composizione del "nastro attivita'" della chat (ADR 0037,
// regola L). Data la timeline per-run gia' disponibile in useChat
// (metaStepsMap + agentStepsMap + traces), questa FUNZIONE PURA la piega in un
// modello ordinato `ActivityStream` = lista di SEGMENTI-PER-PROVIDER, ognuno con
// i propri eventi. Tutti i renderer (live e storico) consumano SOLO questo
// modello: la sequenza e le regole di collasso vivono qui e in nessun altro
// punto. Aggiungere un nuovo tipo di evento significa toccare solo questo file.
//
// Regole vincolanti rispettate:
//  - regola M (segnali strutturati, mai testo): il provider di ogni segmento e'
//    letto dal SEGNALE STRUTTURATO (executor_call.provider / escalation.
//    to_provider / AITraceEvent.provider), l'esito tool da is_error / exit code,
//    MAI dedotto dal parsing del titolo o dell'output umano.
//  - regola G (registry DB): qui non ci sono prezzi/modelli hardcoded; il costo
//    si prezza altrove dal catalogo /api/models.
//
// Campi backend additivi (ADR 0037 sez. 4) sono OPZIONALI: se il payload del
// meta-step `routing` porta provider/model (arricchimento A) lo usiamo, se
// l'escalation porta `cooldown` (arricchimento B) lo mostriamo; in loro assenza
// il modello degrada pulito (segmento provider "unknown" per il routing finche'
// non arriva il primo executor_call/trace, nessun cooldown nella banda switch).

import type { MetaStepEntry } from "./types";
import type { AgentStep, AITraceEvent } from "../api/agent";

// ── Tipi evento del nastro ──────────────────────────────────────────────────
// Ogni evento appartiene a un segmento (che porta provider/model). I tipi sono
// un'unione discriminata su `type` cosi' i renderer fanno switch esaustivo.

export type ToolOutcome = "ok" | "err" | "running";

export interface RoutingEvent {
  type: "routing";
  /** intent classificato (payload.intent del meta-step routing). */
  intent?: string;
  profile?: string;
  behaviorMode?: string;
  tokenBudget?: number;
}

export interface PlanTodo {
  id?: string;
  seq?: number;
  content?: string;
  status?: string;
  priority?: string;
}

export interface PlanEvent {
  type: "plan";
  todos: PlanTodo[];
}

export interface ThoughtEvent {
  type: "thought";
  /** iterazione a cui si riferisce (executor_call.iteration), se nota. */
  iteration?: number;
  text: string;
}

export interface ToolEvent {
  type: "tool";
  name: string;
  /** target leggibile (path/comando), se disponibile. */
  target?: string;
  outcome: ToolOutcome;
  /** exit code strutturato, quando il segnale lo espone. */
  exitCode?: number;
  /** iterazione dell'agent loop, se nota. */
  iteration?: number;
}

export interface SwitchEvent {
  type: "switch";
  fromProvider?: string;
  fromModel?: string;
  toProvider: string;
  toModel?: string;
  reason?: string;
  /** provider `to`/`from` in cooldown (arricchimento B): stringa causa se nota. */
  cooldown?: string;
  /** contatore escalation "n/max" quando il payload lo porta. */
  attempt?: string;
}

export interface VerifyEvent {
  type: "verify";
  /** fase del final_gate (start/passed/failed/forced_close). */
  phase?: string;
  cycle?: number;
  maxCycles?: number;
}

export interface ContextOverflowEvent {
  type: "context_overflow";
  detail?: string;
}

/** Sequenza di tool consecutivi tutti ok, compressa oltre soglia densita'. */
export interface FoldedToolsEvent {
  type: "folded_tools";
  count: number;
  firstIteration?: number;
  lastIteration?: number;
}

export type ActivityEvent =
  | RoutingEvent
  | PlanEvent
  | ThoughtEvent
  | ToolEvent
  | SwitchEvent
  | VerifyEvent
  | ContextOverflowEvent
  | FoldedToolsEvent;

/** Segmento del nastro: tutti gli eventi eseguiti da UN provider/model, in
 *  ordine. Un nuovo segmento si apre a ogni cambio provider effettivo. */
export interface ActivitySegment {
  provider: string;
  model?: string;
  events: ActivityEvent[];
  /** true se il segmento e' stato aperto da uno switch (escalation/fallback):
   *  il renderer antepone la banda "Cambio provider". */
  openedBySwitch: boolean;
  /** dati dello switch che ha aperto il segmento (per la banda). */
  switch?: SwitchEvent;
}

export interface ActivityStream {
  segments: ActivitySegment[];
  /** true se non c'e' alcun segnale (nessun meta-step / step / trace): il
   *  renderer degrada al rendering odierno. */
  empty: boolean;
}

// ── Soglia densita' del collasso ────────────────────────────────────────────
// La soglia N di collasso (>=N tool consecutivi ok -> compressi) arriva come
// PARAMETRO dal renderer, guidato dalla larghezza @container: compatto=2,
// medio=3, esteso=4. Qui la logica e' agnostica alla larghezza.

export type FoldThreshold = 2 | 3 | 4;

// ── Helper di lettura strutturata (regola M) ────────────────────────────────

function asString(v: unknown): string | undefined {
  return typeof v === "string" && v.length > 0 ? v : undefined;
}

function asNumber(v: unknown): number | undefined {
  return typeof v === "number" && Number.isFinite(v) ? v : undefined;
}

/** Esito di uno step agente da SEGNALE STRUTTURATO (status), mai dal testo. */
function stepOutcome(status: AgentStep["status"]): ToolOutcome {
  if (status === "completed") return "ok";
  if (status === "failed" || status === "provider_unavailable") return "err";
  return "running";
}

/** Estrae l'exit code strutturato dal risultato tool, se il payload lo espone
 *  in forma numerica. NON fa parsing del testo umano: cerca solo un pattern
 *  macchina esplicito "exit N" gia' emesso dal tool_result strutturato; in
 *  assenza ritorna undefined (nessuna deduzione dal contenuto). */
function extractExitCode(step: AgentStep): number | undefined {
  const raw = step.toolResult;
  if (!raw) return undefined;
  // Il motore Rust serializza l'esito comando con un marcatore macchina
  // "exit_code=N" / "exit N" all'inizio del risultato: e' un segnale
  // strutturato, non prosa. Se assente non deduciamo nulla.
  const m = /(?:^|\s)exit(?:_code)?[=\s](\d{1,3})\b/.exec(raw.slice(0, 120));
  return m ? Number(m[1]) : undefined;
}

/** Target leggibile di uno step: primo tra path/target/command/file negli input
 *  strutturati. Nessun parsing di prosa. */
function stepTarget(input: Record<string, unknown>): string | undefined {
  for (const key of ["path", "target", "file", "file_path", "command", "cmd", "url"]) {
    const v = input[key];
    if (typeof v === "string" && v.length > 0) return v;
  }
  return undefined;
}

// ── Sorgente unificata degli eventi grezzi ──────────────────────────────────
// Uniamo meta-step e step in una sequenza ordinata per createdAt. Le trace non
// generano eventi propri (evitano doppioni col meta-step executor_call), ma
// forniscono il PROVIDER EFFETTIVO per iterazione: le indicizziamo per usarle
// come sorgente autoritativa del provider di segmento quando il meta-step non
// lo porta (degrado pulito dell'arricchimento A).

type RawKind = "meta" | "step";
interface RawItem {
  kind: RawKind;
  createdAt: string;
  meta?: MetaStepEntry;
  step?: AgentStep;
}

function buildRawTimeline(metaSteps: MetaStepEntry[], steps: AgentStep[]): RawItem[] {
  const items: RawItem[] = [];
  for (const m of metaSteps) {
    items.push({ kind: "meta", createdAt: m.createdAt, meta: m });
  }
  for (const s of steps) {
    items.push({ kind: "step", createdAt: s.createdAt, step: s });
  }
  // Ordinamento stabile per timestamp; a parita' di timestamp i meta-step
  // (decisioni) precedono gli step (esecuzioni) per coerenza narrativa.
  items.sort((a, b) => {
    const ta = Date.parse(a.createdAt) || 0;
    const tb = Date.parse(b.createdAt) || 0;
    if (ta !== tb) return ta - tb;
    if (a.kind === b.kind) return 0;
    return a.kind === "meta" ? -1 : 1;
  });
  return items;
}

/** Indicizza il provider/model EFFETTIVO per iterazione dalle trace gateway
 *  (regola M: provider dal segnale strutturato AITraceEvent.provider). */
function traceByIteration(traces: AITraceEvent[]): Map<number, AITraceEvent> {
  const byIter = new Map<number, AITraceEvent>();
  for (const t of traces) {
    if (typeof t.iteration === "number") byIter.set(t.iteration, t);
  }
  return byIter;
}

// ── Composizione principale ─────────────────────────────────────────────────

/**
 * Piega la timeline di UN run in un `ActivityStream` di segmenti-per-provider.
 *
 * Apertura segmento:
 *  - il primo executor_call/routing/trace apre il segmento iniziale;
 *  - ogni escalation/fallback (o cambio provider effettivo osservato dalle
 *    trace) apre un NUOVO segmento con la banda switch.
 *
 * Collasso (regola densita'): all'interno di un segmento, sequenze di >=
 * `foldThreshold` ToolEvent consecutivi tutti `ok` sono compresse in un unico
 * FoldedToolsEvent. Un tool in errore (`err`) rompe sempre la sequenza e resta
 * visibile: e' un segnale strutturato che l'utente deve vedere.
 *
 * @param metaSteps timeline meta-step del run (metaStepsMap.get(runId))
 * @param steps     step agente del run (agentStepsMap.get(runId))
 * @param traces    trace gateway del run (traces filtrate per runId)
 * @param foldThreshold soglia N di collasso (2 compatto / 3 medio / 4 esteso)
 */
export function composeActivityStream(
  metaSteps: MetaStepEntry[],
  steps: AgentStep[],
  traces: AITraceEvent[],
  foldThreshold: FoldThreshold = 3,
): ActivityStream {
  const raw = buildRawTimeline(metaSteps, steps);
  const traceIter = traceByIteration(traces);
  const firstTrace = traces.length > 0 ? traces[0] : undefined;

  const segments: ActivitySegment[] = [];

  // Segmento corrente: apre lazy al primo evento con un provider noto.
  let current: ActivitySegment | null = null;

  function ensureSegment(provider: string | undefined, model: string | undefined): ActivitySegment {
    const prov = provider ?? current?.provider ?? firstTrace?.provider ?? "unknown";
    const mdl = model ?? (provider ? undefined : current?.model);
    if (!current) {
      current = { provider: prov, model: mdl, events: [], openedBySwitch: false };
      segments.push(current);
    }
    return current;
  }

  function openSwitchSegment(sw: SwitchEvent) {
    current = {
      provider: sw.toProvider,
      model: sw.toModel,
      events: [],
      openedBySwitch: true,
      switch: sw,
    };
    segments.push(current);
  }

  for (const item of raw) {
    if (item.kind === "meta" && item.meta) {
      const m = item.meta;
      const p = m.payload ?? {};
      switch (m.kind) {
        case "routing": {
          // Arricchimento A: provider/model nel payload del routing, se presenti.
          const seg = ensureSegment(asString(p.provider), asString(p.model));
          seg.events.push({
            type: "routing",
            intent: asString(p.intent),
            profile: asString(p.profile_name),
            behaviorMode: asString(p.behavior_mode),
            tokenBudget: asNumber(p.token_budget),
          });
          // Se il routing porta il provider, adotta anche model del segmento
          // (opero sul segmento ritornato, che e' quello corrente).
          const rp = asString(p.provider);
          if (rp) {
            seg.provider = rp;
            seg.model = asString(p.model) ?? seg.model;
          }
          break;
        }
        case "plan": {
          const seg = ensureSegment(undefined, undefined);
          seg.events.push({ type: "plan", todos: (p.todos ?? []) as PlanTodo[] });
          break;
        }
        case "executor_call": {
          // Heartbeat: apre/aggiorna il segmento col provider effettivo. Il
          // provider dal payload (o dalla trace della stessa iterazione, piu'
          // autoritativa se differisce) e' il segnale di segmento.
          const iter = asNumber(p.iteration);
          const tr = iter != null ? traceIter.get(iter) : undefined;
          const prov = tr?.provider ?? asString(p.provider);
          const model = tr?.model ?? asString(p.model);
          const seg = ensureSegment(prov, model);
          // Il provider di ROUTING e' una previsione; la trace e' il fatto
          // (arricchimento B: provider effettivo quando differisce). Se il
          // segmento corrente non e' stato aperto da uno switch e non ha ancora
          // ESEGUITO lavoro (solo eventi decisionali routing/plan/thought, nessun
          // tool), allinealo al provider effettivo. Cosi' un routing che predice
          // google seguito da una prima chiamata effettiva anthropic mostra
          // anthropic come provider del segmento, senza aprire uno switch.
          if (prov && seg.provider !== prov && !seg.openedBySwitch && !hasExecutedWork(seg)) {
            seg.provider = prov;
            seg.model = model ?? seg.model;
          }
          break;
        }
        case "escalation":
        case "fallback": {
          // Cambio provider: apre un nuovo segmento con banda switch.
          const prev = segments[segments.length - 1];
          const toProvider = asString(p.to_provider);
          const toModel = asString(p.to_model);
          const fromProvider = asString(p.from_provider) ?? prev?.provider;
          const fromModel = asString(p.from_model) ?? prev?.model;
          if (!toProvider) {
            // Senza provider di destinazione strutturato non apriamo un segmento
            // fantasma: annotiamo comunque il motivo nel segmento corrente come
            // switch "incompleto" e' evitato (degrado pulito).
            break;
          }
          const sw: SwitchEvent = {
            type: "switch",
            fromProvider,
            fromModel,
            toProvider,
            toModel,
            reason: asString(p.reason),
            // Arricchimento B: causa cooldown se il payload la espone.
            cooldown: asString(p.cooldown) ?? (p.provider_in_cooldown ? asString(p.provider_in_cooldown) : undefined),
            attempt: asString(p.attempt) ?? formatAttempt(asNumber(p.attempt_index), asNumber(p.max_attempts)),
          };
          openSwitchSegment(sw);
          break;
        }
        case "final_gate": {
          const seg = ensureSegment(undefined, undefined);
          seg.events.push({
            type: "verify",
            phase: asString(p.phase),
            cycle: asNumber(p.cycle),
            maxCycles: asNumber(p.max_cycles),
          });
          break;
        }
        case "context_overflow": {
          const seg = ensureSegment(undefined, undefined);
          seg.events.push({ type: "context_overflow", detail: asString(p.reason) ?? m.title });
          break;
        }
        case "reflection": {
          const summary = asString(p.summary);
          if (summary) {
            const seg = ensureSegment(undefined, undefined);
            seg.events.push({ type: "thought", text: summary });
          }
          break;
        }
        default:
          // Altri kind (clarify/next_actions/usage_snapshot/end_turn/...) non
          // fanno parte del nastro attivita': restano gestiti dai renderer
          // dedicati (pulsanti scelte, barra contesto).
          break;
      }
    } else if (item.kind === "step" && item.step) {
      const s = item.step;
      // Gli step "supervisor_check" sono meta-verifiche interne, non tool utente.
      if (s.toolName === "supervisor_check") continue;
      const tr = traceIter.get(s.stepIndex);
      const seg = ensureSegment(tr?.provider, tr?.model);
      seg.events.push({
        type: "tool",
        name: s.toolName,
        target: stepTarget(s.toolInput),
        outcome: stepOutcome(s.status),
        exitCode: extractExitCode(s),
        iteration: s.stepIndex,
      });
    }
  }

  // Applica il collasso dei tool consecutivi ok dentro ogni segmento.
  for (const seg of segments) {
    seg.events = foldConsecutiveOkTools(seg.events, foldThreshold);
  }

  return { segments, empty: segments.length === 0 };
}

/** true se il segmento ha gia' ESEGUITO lavoro (tool/folded), non solo eventi
 *  decisionali (routing/plan/thought/verify). Usato per decidere se il provider
 *  effettivo di una trace puo' ancora ri-allineare il provider di segmento. */
function hasExecutedWork(seg: ActivitySegment): boolean {
  return seg.events.some((e) => e.type === "tool" || e.type === "folded_tools");
}

/** Formatta il contatore escalation "n/max" quando entrambi noti. */
function formatAttempt(index?: number, max?: number): string | undefined {
  if (index == null) return undefined;
  return max != null ? `${index}/${max}` : String(index);
}

/**
 * Comprime sequenze di >= threshold ToolEvent consecutivi tutti `ok` in un
 * unico FoldedToolsEvent. Un tool in errore o `running` interrompe la sequenza
 * e resta sempre visibile (regola M: l'esito strutturato non si nasconde mai).
 * Sequenze piu' corte della soglia restano espanse.
 */
export function foldConsecutiveOkTools(
  events: ActivityEvent[],
  threshold: FoldThreshold,
): ActivityEvent[] {
  const out: ActivityEvent[] = [];
  let run: ToolEvent[] = [];

  const flush = () => {
    if (run.length >= threshold) {
      out.push({
        type: "folded_tools",
        count: run.length,
        firstIteration: run[0].iteration,
        lastIteration: run[run.length - 1].iteration,
      });
    } else {
      out.push(...run);
    }
    run = [];
  };

  for (const ev of events) {
    if (ev.type === "tool" && ev.outcome === "ok") {
      run.push(ev);
    } else {
      flush();
      out.push(ev);
    }
  }
  flush();
  return out;
}

/** Filtra le trace di un singolo run (le trace in useChat sono per-sessione). */
export function tracesForRun(traces: AITraceEvent[], runId: string): AITraceEvent[] {
  return traces.filter((t) => t.runId === runId);
}

// ── Aggregazione costo-per-provider (regola G: prezzi dal catalogo) ──────────
// Aggrega SOLO i token per provider/model dalle trace del run. Il PREZZO viene
// applicato dal renderer col catalogo /api/models (useModelPricing di
// provider-badge.tsx): qui NON esiste alcun prezzo hardcoded.

export interface ProviderTokenBucket {
  provider: string;
  model: string;
  inputTokens: number;
  outputTokens: number;
}

/** Somma input/output token per coppia provider/model dalle trace del run. */
export function aggregateTokensByProvider(traces: AITraceEvent[]): ProviderTokenBucket[] {
  const map = new Map<string, ProviderTokenBucket>();
  for (const t of traces) {
    const key = `${t.provider} ${t.model}`;
    const bucket = map.get(key) ?? {
      provider: t.provider,
      model: t.model,
      inputTokens: 0,
      outputTokens: 0,
    };
    bucket.inputTokens += t.inputTokens ?? 0;
    bucket.outputTokens += t.outputTokens ?? 0;
    map.set(key, bucket);
  }
  return Array.from(map.values());
}
