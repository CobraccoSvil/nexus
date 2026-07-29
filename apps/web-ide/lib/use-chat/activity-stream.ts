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

/** Provenienza dell'evento: provider/model che l'hanno eseguito. Stampata sugli
 *  eventi in composeActivityStream cosi' OGNI riga del nastro puo' mostrare
 *  l'icona provider + tooltip modello, senza risalire al segmento. Per i tool il
 *  model puo' essere quello EFFETTIVO della trace della stessa iterazione (piu'
 *  preciso su upscale intra-segmento). Le bande switch NON la usano (hanno i
 *  ProviderBadge). */
export interface EventProvenance {
  provider?: string;
  model?: string;
  /** Ancora strutturale dell'evento nel nastro (posizione canonica segmento/
   *  evento), assegnata UNA volta a fine composeActivityStream. Consumata dal
   *  renderer (id DOM) e dalla campanella (deep-link). Formato definito dagli
   *  helper esportati activityLocalAnchorId/runScopedAnchorId (regola L). */
  anchorId?: string;
}

export interface RoutingEvent extends EventProvenance {
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

export interface PlanEvent extends EventProvenance {
  type: "plan";
  todos: PlanTodo[];
}

export interface ThoughtEvent extends EventProvenance {
  type: "thought";
  /** iterazione a cui si riferisce (executor_call.iteration), se nota. */
  iteration?: number;
  text: string;
}

export interface ToolEvent extends EventProvenance {
  type: "tool";
  name: string;
  /** target leggibile (path/comando), se disponibile. */
  target?: string;
  outcome: ToolOutcome;
  /** exit code strutturato, quando il segnale lo espone. */
  exitCode?: number;
  /** iterazione dell'agent loop, se nota. */
  iteration?: number;
  /** input strutturato del tool (toolInput dello step): per l'espansione
   *  "Parametri" della riga tool nel nastro. */
  input?: Record<string, unknown>;
  /** risultato/errore del tool (toolResult dello step): per l'espansione
   *  "Risultato/Errore" della riga tool (troncato in UI). */
  result?: string;
}

export interface SwitchEvent {
  type: "switch";
  fromProvider?: string;
  fromModel?: string;
  toProvider: string;
  toModel?: string;
  /** Identificatore canonico del motivo (`final_gate_nonconvergence`,
   *  `signature_loop`, ...): serve alla logica e ai test, NON all'occhio. */
  reason?: string;
  /** La frase che spiega il motivo, composta dal backend dove il vocabolario e'
   *  definito (`decisions::switch_reason::SwitchReason::descrizione`).
   *
   *  Additiva: senza di essa il renderer degradava al codice grezzo dentro un
   *  `<code>`, e nella card si leggeva `Motivo: final_gate_nonconvergence`. La
   *  descrizione NON viene tenuta qui in una tabella parallela: sarebbe la copia
   *  scritta a mano che diverge al primo motivo nuovo -- e' esattamente cio' che
   *  era gia' successo con SWITCH_CAUSE_LABELS. */
  reasonDescription?: string;
  /** Causa STRUTTURATA dello switch (vocabolario chiuso del backend,
   *  ProviderFailureCause: cooldown | billing | client_error |
   *  policy_tier_excluded | unknown). Il renderer la mappa in etichetta umana
   *  onesta invece di mostrare il codice grezzo `provider_failover`. */
  cause?: string;
  /** provider `to`/`from` in cooldown (arricchimento B): stringa causa se nota. */
  cooldown?: string;
  /** contatore escalation "n/max" quando il payload lo porta. */
  attempt?: string;
}

export interface VerifyEvent extends EventProvenance {
  type: "verify";
  /** fase del final_gate (start/passed/failed/forced_close). */
  phase?: string;
  cycle?: number;
  maxCycles?: number;
}

export interface ContextOverflowEvent extends EventProvenance {
  type: "context_overflow";
  detail?: string;
}

/** Esito del ReviewGate del grafo (kind=review_gate, nodo review_gate.rs):
 *  rimando in correzione, chiusura bocciata al cap, o verdetto di chiusura
 *  (pass/inconclusive). Il `title` e' gia' composto dal backend; phase e
 *  verdict sono i segnali strutturati del payload (regola M). */
export interface ReviewGateEvent extends EventProvenance {
  type: "review_gate";
  /** fase strutturata: closed | failed | rejected_final. */
  phase?: string;
  /** verdetto del panel: pass | fail | needs_changes | inconclusive. */
  verdict?: string;
  cycle?: number;
  maxCycles?: number;
  /** titolo leggibile del meta-step (composto dal backend). */
  title: string;
  /** Chi ha votato. Stessa forma di `panelProviders` (provider e modello
   *  SEPARATI): il nome del modello puo' contenere `/` (`z-ai/glm-4.7-flash`),
   *  quindi una stringa `provider/modello` costringerebbe a indovinare dove
   *  tagliare. Senza questo campo la riga REVIEW mostra l'icona del run PADRE,
   *  e un panel su piu' provider sembra girato tutto sullo stesso. */
  reviewers?: PanelProviderEntry[];
}

/** Sequenza di tool consecutivi tutti ok, compressa oltre soglia densita'.
 *  I ToolEvent originali sono CONSERVATI in `tools` (non scartati) cosi' il
 *  renderer puo' espandere la riga collassata e mostrare i singoli tool (ognuno
 *  a sua volta espandibile per Parametri/Risultato). Niente troncamento
 *  silenzioso: il folding e' sempre reversibile con un click. */
export interface FoldedToolsEvent extends EventProvenance {
  type: "folded_tools";
  count: number;
  firstIteration?: number;
  lastIteration?: number;
  tools: ToolEvent[];
}

/** Narrazione di un SUB-AGENTE dispatchato dal run (meta-step subagent_started/
 *  progress/completed/failed emessi dal ponte in subagent_native.rs, mig 0535).
 *  La `phase` e' il segnale strutturato del payload (regola M), il `title` e'
 *  gia' composto dal backend. Gli eventi dello stesso sub-run condividono
 *  `subagentRunId` (= correlation_id del meta-step). */
export interface SubagentEvent extends EventProvenance {
  type: "subagent";
  phase: "started" | "tool" | "working" | "completed" | "failed";
  subagentKind?: string;
  subagentRunId?: string;
  /** titolo leggibile del meta-step (composto dal backend). */
  title: string;
  /** dettagli tool inoltrato (phase="tool"). */
  tool?: string;
  target?: string;
  isError?: boolean;
  /** dettagli di chiusura (phase="completed"/"failed"). */
  summary?: string;
  iterations?: number;
  costUsd?: number;
  /** secondi di lavoro (phase="working", heartbeat). */
  elapsedS?: number;
}

/** Il run PADRE e' entrato in `awaiting_subagents` (fan-in async): ha
 *  dispatchato N sub-agent in background e il backend lo ha SOSPESO in attesa
 *  che completino, poi lo riprende (flag ora OFF lato backend). NON e' uno stato
 *  terminale: lo stream resta aperto e la narrazione dei sub-agent (SubagentEvent)
 *  continua ad arrivare. Questo evento e' il segnale di "attesa" da mostrare nel
 *  nastro, distinto dai singoli SubagentEvent (che raccontano il lavoro di OGNI
 *  figlio). Estende EventProvenance: e' un evento del PADRE, riceve il provider
 *  del segmento padre come gli altri eventi non-subagent.
 *
 *  Contratto backend confermato: il meta-step ha kind === "awaiting_subagents" e
 *  payload { status, pending_count: <numero>, run_id }. Il contatore e' letto da
 *  `payload.pending_count` (readAwaitingCount, punto unico). */
export interface AwaitingSubagentsEvent extends EventProvenance {
  type: "awaiting_subagents";
  /** numero di sub-agent ancora in attesa (payload.pending_count), se presente. */
  count?: number;
  title: string;
}

/** Parere strutturato completo di una figura (dallo structured output del tool
 *  advisory_verdict). Propagato dal backend per far LEGGERE il testo di ogni parere. */
export interface FigureAdvisory {
  verdict?: string;
  requirements?: string[];
  risks?: Array<Record<string, unknown>>;
  recommendations?: string[];
  concerns?: string[];
}

/** Report per-figura del consiglio (segnale strutturato backend). */
export interface FigureAdvisoryReport {
  kind: string;
  status:
    | "prepare_failed"
    | "run_failed"
    | "run_timeout"
    | "completed_no_advisory"
    | "invalid_advisory"
    | "advisory_ok";
  detail_code: string;
  detail_message: string;
  advisory_verdict?: string;
  advisory?: FigureAdvisory;
  /** Provider/model EFFETTIVI su cui la figura ha girato (provenienza del parere).
   *  Assenti per le figure respinte a monte (guard depth) senza modello risolto. */
  provider?: string;
  model?: string;
  subagent_run_id?: string;
}

export type CouncilFigureTaskStatus = "pending" | "running" | "done" | "failed";

export interface CouncilFigureTask {
  kind: string;
  status: CouncilFigureTaskStatus;
}

/** Il run ha attivato il Consiglio delle Competenze tramite analisi agentica/
 *  deterministica di complessita' e ambito. Non e' un toggle manuale: la fonte
 *  e' il meta-step backend `council_of_competencies`. */
export interface CouncilOfCompetenciesEvent extends EventProvenance {
  type: "council_of_competencies";
  title: string;
  productName: string;
  activationSource?: string;
  phase?: "convening" | "complete";
  completedCount?: number;
  figureCount?: number;
  figureTasks?: CouncilFigureTask[];
  degraded?: boolean;
  degradationReason?: string;
  figureReports?: FigureAdvisoryReport[];
}

/** Coppia provider/model di un analista del panel multi-provider (payload backend). */
export interface PanelProviderEntry {
  provider: string;
  model?: string;
}

/** Panel multi-provider: stesso problema analizzato da provider/modelli distinti
 *  tramite purpose tier-aware. Meta-step backend `multi_provider_panel`. */
export interface MultiProviderPanelEvent extends EventProvenance {
  type: "multi_provider_panel";
  title: string;
  productName: string;
  activationSource?: string;
  providerCount?: number;
  panelProviders?: PanelProviderEntry[];
  degraded?: boolean;
  degradationReason?: string;
  /** Parere individuale di ciascun provider (stessa shape dei report di figura):
   *  espandibile in UI per mostrare la differenza tra i provider. */
  providerReports?: FigureAdvisoryReport[];
}

export type ActivityEvent =
  | RoutingEvent
  | PlanEvent
  | ThoughtEvent
  | ToolEvent
  | SwitchEvent
  | VerifyEvent
  | ReviewGateEvent
  | ContextOverflowEvent
  | FoldedToolsEvent
  | SubagentEvent
  | AwaitingSubagentsEvent
  | CouncilOfCompetenciesEvent
  | MultiProviderPanelEvent;

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
  /** Eventi non-switch di QUESTO segmento nascosti dal cap live (`capStreamToRecent`).
   *  > 0 -> il renderer mostra "N passi precedenti" cosi' il provider non sparisce
   *  del tutto dalla vista live (i suoi step sono nello storico, visibili al refresh). */
  cappedCount?: number;
  /** Ancora strutturale del SEGMENTO (posizione canonica), assegnata a fine
   *  composeActivityStream su OGNI segmento. Il renderer la stampa sulla banda
   *  "Cambio provider" e sul placeholder "N passi precedenti"; la campanella la
   *  usa come fallback quando l'evento puntato e' stato cappato dalla vista live
   *  (il segmento resta sempre nel DOM, l'evento no). */
  anchorId?: string;
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

function asStringList(v: unknown): string[] | undefined {
  if (!Array.isArray(v)) return undefined;
  const out = v.filter((x): x is string => typeof x === "string" && x.trim().length > 0);
  return out.length > 0 ? out : undefined;
}

/** Parere strutturato completo di una figura dal payload backend (regola M:
 *  leggiamo i campi strutturati, non prosa). `undefined` se non c'e' contenuto. */
function readFigureAdvisory(v: unknown): FigureAdvisory | undefined {
  if (!v || typeof v !== "object") return undefined;
  const a = v as Record<string, unknown>;
  const risks = Array.isArray(a.risks)
    ? a.risks.filter((r): r is Record<string, unknown> => !!r && typeof r === "object")
    : undefined;
  const advisory: FigureAdvisory = {
    verdict: asString(a.verdict),
    requirements: asStringList(a.requirements),
    recommendations: asStringList(a.recommendations),
    concerns: asStringList(a.concerns),
    risks: risks && risks.length > 0 ? risks : undefined,
  };
  const hasContent =
    advisory.verdict ||
    advisory.requirements ||
    advisory.recommendations ||
    advisory.concerns ||
    advisory.risks;
  return hasContent ? advisory : undefined;
}

function readFigureReports(
  payload: Record<string, unknown>,
  key: "figure_reports" | "provider_reports" = "figure_reports",
): FigureAdvisoryReport[] | undefined {
  const raw = payload[key];
  if (!Array.isArray(raw) || raw.length === 0) return undefined;
  const out: FigureAdvisoryReport[] = [];
  for (const item of raw) {
    if (!item || typeof item !== "object") continue;
    const r = item as Record<string, unknown>;
    const kind = asString(r.kind);
    const status = asString(r.status);
    const detailCode = asString(r.detail_code);
    const detailMessage = asString(r.detail_message);
    if (!kind || !status || !detailCode || !detailMessage) continue;
    if (
      status !== "prepare_failed" &&
      status !== "run_failed" &&
      status !== "run_timeout" &&
      status !== "completed_no_advisory" &&
      status !== "invalid_advisory" &&
      status !== "advisory_ok"
    ) {
      continue;
    }
    out.push({
      kind,
      status,
      detail_code: detailCode,
      detail_message: detailMessage,
      advisory_verdict: asString(r.advisory_verdict),
      advisory: readFigureAdvisory(r.advisory),
      provider: asString(r.provider),
      model: asString(r.model),
      subagent_run_id: asString(r.subagent_run_id),
    });
  }
  return out.length > 0 ? out : undefined;
}

/** Tono visivo del parere di una figura; il componente lo mappa a un colore
 *  concreto (la palette resta presentazione, la decisione resta qui). */
export type FigureVerdictTone =
  | "proceed"
  | "changes"
  | "block"
  | "invalid"
  | "technical"
  | "unknown";

/** Etichetta + tono del parere di UNA figura, pronti per il tag del display. */
export interface FigureVerdictDisplay {
  tone: FigureVerdictTone;
  label: string;
}

/** Etichetta di un parere `advisory_ok` dal verdetto canonico (regola N:
 *  proceed | proceed_with_changes | block). `advisory_ok` GARANTISCE alla fonte
 *  un verdetto canonico: se manca e' un'incoerenza del backend, non un'astensione
 *  tecnica, e resta "n/d" (tono unknown) senza fingere un parere. */
function advisoryOkDisplay(verdict: string | undefined): FigureVerdictDisplay {
  switch (verdict) {
    case "proceed":
      return { tone: "proceed", label: "procede" };
    case "proceed_with_changes":
      return { tone: "changes", label: "procede con modifiche" };
    case "block":
      return { tone: "block", label: "blocca" };
    default:
      return { tone: "unknown", label: "n/d" };
  }
}

/** Etichetta + tono del parere di UNA figura del consiglio a partire dal SEGNALE
 *  STRUTTURATO `report.status` (enum backend), MAI dalla prosa (regola M).
 *  Distingue un parere ESPRESSO (procede / con modifiche / blocca) da
 *  un'astensione TECNICA con causa nota (tempo scaduto, errore, nessun parere,
 *  non avviata) e da un parere INVALIDO. Cosi' il display non collassa cause
 *  diverse in un opaco "n/d": una figura in timeout dice "tempo scaduto", una che
 *  veta dice "blocca", e le due non si confondono. Un `block` con evidenza NON e'
 *  declassato: arriva qui come `advisory_ok` con `advisory.verdict = "block"`.
 *  Punto unico (regola L) letto sia dai report di figura sia da quelli di
 *  provider del panel multi-provider. */
export function figureVerdictDisplay(report: FigureAdvisoryReport): FigureVerdictDisplay {
  switch (report.status) {
    case "advisory_ok":
      return advisoryOkDisplay(report.advisory?.verdict ?? report.advisory_verdict);
    case "run_timeout":
      return { tone: "technical", label: "tempo scaduto" };
    case "run_failed":
      return { tone: "technical", label: "errore" };
    case "prepare_failed":
      return { tone: "technical", label: "non avviata" };
    case "completed_no_advisory":
      return { tone: "technical", label: "nessun parere" };
    case "invalid_advisory":
      return { tone: "invalid", label: "parere non valido" };
    default:
      return { tone: "unknown", label: "n/d" };
  }
}

function readCouncilFigureTasks(
  payload: Record<string, unknown>,
): CouncilFigureTask[] | undefined {
  const raw = payload.figure_tasks;
  if (!Array.isArray(raw) || raw.length === 0) return undefined;
  const out: CouncilFigureTask[] = [];
  for (const item of raw) {
    if (!item || typeof item !== "object") continue;
    const r = item as Record<string, unknown>;
    const kind = asString(r.kind);
    const status = asString(r.status);
    if (!kind || !status) continue;
    if (
      status !== "pending" &&
      status !== "running" &&
      status !== "done" &&
      status !== "failed"
    ) {
      continue;
    }
    out.push({ kind, status });
  }
  return out.length > 0 ? out : undefined;
}

/** Legge una lista "chi ha eseguito" dal payload. Una sola funzione per due
 *  chiavi (`panel_providers` del panel multi-provider, `reviewers` del review
 *  gate): stesso concern, stessa forma `{provider, model}`, stesso lettore. */
function readPanelProviders(
  payload: Record<string, unknown>,
  key: string = "panel_providers",
): PanelProviderEntry[] | undefined {
  const raw = payload[key];
  if (!Array.isArray(raw) || raw.length === 0) return undefined;
  const out: PanelProviderEntry[] = [];
  for (const item of raw) {
    if (!item || typeof item !== "object") continue;
    const r = item as Record<string, unknown>;
    const provider = asString(r.provider);
    if (!provider) continue;
    out.push({ provider, model: asString(r.model) });
  }
  return out.length > 0 ? out : undefined;
}

function upsertCouncilEvent(seg: ActivitySegment, ev: CouncilOfCompetenciesEvent): void {
  for (let i = seg.events.length - 1; i >= 0; i--) {
    const prev = seg.events[i];
    if (prev.type !== "council_of_competencies") continue;
    if (ev.phase === "convening" && prev.phase === "convening") {
      seg.events[i] = ev;
      return;
    }
    if (ev.phase !== "convening" && prev.phase === "convening") {
      seg.events[i] = ev;
      return;
    }
    break;
  }
  seg.events.push(ev);
}

/** Legge il numero di sub-agent in attesa dal payload del meta-step
 *  `awaiting_subagents` (segnale strutturato, regola M). PUNTO UNICO del
 *  contatore. Contratto backend confermato: il campo e' `pending_count`. */
function readAwaitingCount(payload: Record<string, unknown>): number | undefined {
  return asNumber(payload.pending_count);
}

/** Contatore sub-agent in attesa dall'ULTIMO meta-step `awaiting_subagents` del
 *  run (segnale strutturato, regola M). Fonte primaria dello STATO e' comunque
 *  `agentRun.status === "awaiting_subagents"`: questo helper serve solo ad
 *  arricchire il banner col numero, se disponibile. Ritorna `undefined` se non
 *  c'e' il meta-step o il payload non porta il conteggio (banner generico). */
export function latestAwaitingSubagentsCount(
  metaSteps: MetaStepEntry[],
): number | undefined {
  for (let i = metaSteps.length - 1; i >= 0; i--) {
    const m = metaSteps[i];
    if (m.kind === "awaiting_subagents") {
      return readAwaitingCount(m.payload ?? {});
    }
  }
  return undefined;
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

/** Estrae nome + input REALI di uno step gestendo DUE forme:
 *  - SSE: `toolName` valorizzato, `toolInput` = parametri diretti;
 *  - DB (getAgentRun): `toolName` VUOTO, nome e parametri annidati in
 *    `toolInput = { tool_name, tool_input: {...} }`.
 *  Il `||`/`??` copre entrambe senza rompere il caso SSE. */
function unwrapStep(step: AgentStep): { name: string; input: Record<string, unknown> } {
  const rawInput = (step.toolInput ?? {}) as Record<string, unknown>;
  const nested = rawInput.tool_input;
  const name =
    (step.toolName && step.toolName.length > 0 ? step.toolName : undefined) ??
    (typeof rawInput.tool_name === "string" ? rawInput.tool_name : undefined) ??
    "";
  const input =
    nested && typeof nested === "object" && !Array.isArray(nested)
      ? (nested as Record<string, unknown>)
      : rawInput;
  return { name, input };
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
            reasonDescription: asString(p.reason_description),
            // Causa strutturata dello switch (regola M): vocabolario chiuso
            // emesso dall'executor (payload.cause), mai dedotta dal titolo.
            cause: asString(p.cause),
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
        case "review_gate": {
          const seg = ensureSegment(undefined, undefined);
          seg.events.push({
            type: "review_gate",
            phase: asString(p.phase),
            verdict: asString(p.verdict),
            reviewers: readPanelProviders(p, "reviewers"),
            cycle: asNumber(p.cycle),
            maxCycles: asNumber(p.max_cycles),
            title: m.title,
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
        case "subagent_started":
        case "subagent_progress":
        case "subagent_completed":
        case "subagent_failed": {
          // Narrazione sub-agente (ponte subagent_native.rs): la fase viene dal
          // SEGNALE STRUTTURATO kind + payload.phase (regola M), mai dal titolo.
          const seg = ensureSegment(undefined, undefined);
          const phase: SubagentEvent["phase"] =
            m.kind === "subagent_started"
              ? "started"
              : m.kind === "subagent_completed"
                ? "completed"
                : m.kind === "subagent_failed"
                  ? "failed"
                  : asString(p.phase) === "working"
                    ? "working"
                    : "tool";
          const ev: SubagentEvent = {
            type: "subagent",
            phase,
            subagentKind: asString(p.subagent_kind),
            subagentRunId: asString(p.subagent_run_id) ?? m.correlationId ?? undefined,
            title: m.title,
            tool: asString(p.tool),
            target: asString(p.target),
            isError: p.is_error === true || m.kind === "subagent_failed",
            summary: asString(p.summary) ?? asString(p.error),
            iterations: asNumber(p.iterations),
            costUsd: asNumber(p.cost_usd),
            elapsedS: asNumber(p.elapsed_s),
            // Provenienza del FIGLIO dal payload del ponte (regola M): il pin
            // allo start se noto, poi il provider corrente del sub-run stampato
            // sui progress/chiusura. MAI il provider del segmento padre.
            provider: asString(p.provider),
            model: asString(p.model),
          };
          // Heartbeat "al lavoro": tiene UN SOLO working per sub-run, sempre in
          // coda (l'elapsed piu' recente). Cerchiamo il precedente working DELLO
          // STESSO subagentRunId ovunque sia (non solo l'ultimo evento): col
          // batch parallelo gli heartbeat di sub-run diversi si interlacciano e
          // un confronto col solo `last` non li comprimerebbe. Rimuovendo il
          // vecchio e ri-accodando il nuovo si mantiene un working per run e
          // l'ordine cronologico. Keep-alive: nessuna informazione persa.
          if (phase === "working") {
            const prevIdx = seg.events.findIndex(
              (e) =>
                e.type === "subagent" &&
                e.phase === "working" &&
                e.subagentRunId === ev.subagentRunId,
            );
            if (prevIdx >= 0) seg.events.splice(prevIdx, 1);
          }
          seg.events.push(ev);
          break;
        }
        case "awaiting_subagents": {
          // Il run PADRE si e' sospeso in attesa dei sub-agent in background
          // (fan-in async). Il contatore arriva dal SEGNALE STRUTTURATO del
          // payload (payload.pending_count, regola M), mai dal parsing del titolo.
          const seg = ensureSegment(undefined, undefined);
          seg.events.push({
            type: "awaiting_subagents",
            count: readAwaitingCount(p),
            title: m.title,
          });
          break;
        }
        case "council_of_competencies": {
          // Indicatore prodotto: il Consiglio delle Competenze e' stato attivato
          // da un gate strutturato a monte, non da un toggle manuale o dal testo
          // del modello.
          const seg = ensureSegment(undefined, undefined);
          const signal = asString(p.signal);
          const phase =
            signal === "council_convening" || p.phase === "convening"
              ? ("convening" as const)
              : ("complete" as const);
          upsertCouncilEvent(seg, {
            type: "council_of_competencies",
            title: m.title,
            productName: asString(p.product_name) ?? "Consiglio delle Competenze",
            activationSource: asString(p.activation_source),
            phase,
            completedCount: asNumber(p.completed_count),
            figureCount: asNumber(p.figure_count),
            figureTasks: readCouncilFigureTasks(p),
            degraded: p.degraded === true,
            degradationReason:
              asString(p.degradation_detail) ?? asString(p.degradation_reason),
            figureReports: readFigureReports(p),
          });
          break;
        }
        case "multi_provider_panel": {
          const seg = ensureSegment(undefined, undefined);
          seg.events.push({
            type: "multi_provider_panel",
            title: m.title,
            productName: asString(p.product_name) ?? "Multi-provider advisory",
            activationSource: asString(p.activation_source),
            providerCount: typeof p.provider_count === "number" ? p.provider_count : undefined,
            panelProviders: readPanelProviders(p),
            providerReports: readFigureReports(p, "provider_reports"),
            degraded: p.degraded === true,
            degradationReason:
              asString(p.degradation_detail) ?? asString(p.degradation_reason),
          });
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
      // Unwrap nome + input REALI: gli step storici (DB) annidano nome e
      // parametri in toolInput = { tool_name, tool_input }, con toolName vuoto.
      const { name, input: realInput } = unwrapStep(s);
      // Gli step "supervisor_check" sono meta-verifiche interne, non tool utente.
      if (name === "supervisor_check") continue;
      const tr = traceIter.get(s.stepIndex);
      const seg = ensureSegment(tr?.provider, tr?.model);
      seg.events.push({
        type: "tool",
        name,
        target: stepTarget(realInput),
        outcome: stepOutcome(s.status),
        exitCode: extractExitCode(s),
        iteration: s.stepIndex,
        // Dettaglio per l'espansione della riga tool: parametri VERI (senza
        // wrapper) + risultato grezzo (umanizzato nel renderer).
        input: realInput,
        result: s.toolResult,
      });
    }
  }

  // Provider/model del run PADRE (ultimo segmento con provider noto): usato per
  // eventi prodotto (Consiglio, multi-provider panel) in segmenti "unknown".
  const primaryRun = resolvePrimaryRunProvenance(segments);

  // Stampa la PROVENIENZA (provider/model) su ogni evento del segmento, cosi'
  // ogni riga del nastro puo' mostrare l'icona provider + tooltip modello senza
  // risalire al segmento. Per i TOOL usa il model EFFETTIVO della trace della
  // stessa iterazione (piu' preciso su upscale intra-segmento); altrimenti il
  // model del segmento. Le bande switch NON la ricevono (hanno i ProviderBadge).
  // Gli eventi SUBAGENT sono esclusi: la loro provenienza e' quella del FIGLIO
  // (dal payload del ponte), ereditare il provider del segmento PADRE sarebbe
  // un'attribuzione falsa (il figlio ha il proprio routing).
  // Va fatto PRIMA del folding cosi' i tool interni al folded hanno gia' la
  // provenienza; il FoldedToolsEvent la eredita dal segmento subito dopo.
  for (const seg of segments) {
    for (const ev of seg.events) {
      if (ev.type === "switch" || ev.type === "subagent") continue;
      const productEvent =
        ev.type === "council_of_competencies" || ev.type === "multi_provider_panel";
      const prov =
        seg.provider !== "unknown"
          ? seg.provider
          : productEvent && primaryRun?.provider
            ? primaryRun.provider
            : seg.provider;
      const segModel =
        seg.provider !== "unknown"
          ? seg.model
          : productEvent && primaryRun?.provider
            ? primaryRun.model
            : seg.model;
      ev.provider = prov;
      if (ev.type === "tool") {
        const tr = ev.iteration != null ? traceIter.get(ev.iteration) : undefined;
        ev.model = tr?.model ?? segModel;
      } else {
        ev.model = segModel;
      }
    }
  }

  // Propaga la provenienza del FIGLIO tra gli eventi dello stesso sub-run.
  propagateSubagentProvenance(segments);

  // Applica il collasso dei tool consecutivi ok dentro ogni segmento.
  for (const seg of segments) {
    seg.events = foldConsecutiveOkTools(seg.events, foldThreshold);
    // Il FoldedToolsEvent eredita la provenienza del segmento (i singoli tool
    // conservati mantengono la propria, gia' stampata sopra).
    for (const ev of seg.events) {
      if (ev.type === "folded_tools") {
        ev.provider = seg.provider;
        ev.model = seg.model;
      }
    }
  }

  // ── Ancoraggio (regola L) ──────────────────────────────────────────────────
  // ULTIMO passo, dopo il folding: assegna l'ancora canonica UNA sola volta su
  // ogni segmento e ogni evento (posizione strutturale, mai dedotta dal testo,
  // regola M). L'id STORATO qui e' letto identico dal renderer (attributo DOM) e
  // dalla campanella (deep-link), quindi le due letture coincidono per
  // costruzione. Sopravvive a `capStreamToRecent` (spread dei segmenti + stessi
  // ref evento): l'ancora resta il valore pre-cap, mai l'indice del render.
  for (let si = 0; si < segments.length; si++) {
    const seg = segments[si];
    seg.anchorId = segmentAnchorId(si);
    for (let ei = 0; ei < seg.events.length; ei++) {
      const ev = seg.events[ei];
      // Gli switch vivono in `seg.switch` (banda a livello segmento), non negli
      // `events` resi come riga: la loro ancora e' quella del segmento.
      if (ev.type === "switch") continue;
      ev.anchorId = activityLocalAnchorId(si, ei);
    }
  }

  return { segments, empty: segments.length === 0 };
}

// ── Ancoraggio: formato canonico degli id (regola L, punto unico) ────────────
// Il TEMPLATE degli id vive SOLO qui: renderer e campanella li leggono da questi
// helper, mai ricodificano la stringa altrove. `activityLocalAnchorId`/
// `segmentAnchorId` sono le ancore LOCALI storate sugli oggetti; `runScopedAnchorId`
// le combina col runId al confine DOM (in cronologia coesistono piu' nastri, gli
// id vanno scopati per run per non colpire il turno sbagliato).

/** Ancora locale di UN evento: posizione canonica (segmento, evento). */
export function activityLocalAnchorId(segIndex: number, evIndex: number): string {
  return `seg${segIndex}-ev${evIndex}`;
}

/** Ancora locale di UN segmento (banda switch / placeholder passi cappati). */
export function segmentAnchorId(segIndex: number): string {
  return `seg${segIndex}`;
}

/** Id DOM completo: ancora locale scopata per run. */
export function runScopedAnchorId(runId: string, local: string): string {
  return `nx-as-${runId}-${local}`;
}

/** true se il segmento ha gia' ESEGUITO lavoro (tool/folded), non solo eventi
 *  decisionali (routing/plan/thought/verify). Usato per decidere se il provider
 *  effettivo di una trace puo' ancora ri-allineare il provider di segmento. */
function hasExecutedWork(seg: ActivitySegment): boolean {
  return seg.events.some((e) => e.type === "tool" || e.type === "folded_tools");
}

/** Ultimo provider/model noto del run padre (esclude "unknown"). */
function resolvePrimaryRunProvenance(
  segments: ActivitySegment[],
): EventProvenance | undefined {
  let last: EventProvenance | undefined;
  for (const seg of segments) {
    if (seg.provider && seg.provider !== "unknown") {
      last = { provider: seg.provider, model: seg.model };
    }
  }
  return last;
}

/**
 * Propaga la provenienza (provider/model del FIGLIO) tra gli eventi dello
 * stesso sub-run (chiave: subagentRunId). Lo `started` e' emesso PRIMA che il
 * routing del figlio scelga il modello: quando il primo progress porta il
 * provider (executor_call del figlio via ponte), la ricomposizione lo stampa
 * retroattivamente anche sullo started ("il blocco si aggiorna"). I progress
 * senza provenienza (heartbeat precedenti a nuovi segnali) ereditano l'ultimo
 * provider noto del run. Solo se il provider resta DAVVERO ignoto degrada a
 * "unknown" -> icona '?' (mai il provider del segmento padre).
 */
function propagateSubagentProvenance(segments: ActivitySegment[]): void {
  const subEvents: SubagentEvent[] = [];
  for (const seg of segments) {
    for (const ev of seg.events) {
      if (ev.type === "subagent") subEvents.push(ev);
    }
  }
  // Forward: gli eventi senza provenienza ereditano l'ultimo noto del run.
  const lastKnown = new Map<string, EventProvenance>();
  for (const ev of subEvents) {
    const key = ev.subagentRunId ?? "";
    if (ev.provider) {
      lastKnown.set(key, { provider: ev.provider, model: ev.model });
    } else {
      const known = lastKnown.get(key);
      if (known) {
        ev.provider = known.provider;
        ev.model = known.model;
      }
    }
  }
  // Backward: lo started (prima del primo executor_call del figlio) eredita
  // dal PRIMO evento successivo con provenienza nota; se nessuno la porta,
  // il run e' davvero ignoto -> "unknown".
  const nextKnown = new Map<string, EventProvenance>();
  for (let i = subEvents.length - 1; i >= 0; i--) {
    const ev = subEvents[i];
    const key = ev.subagentRunId ?? "";
    if (ev.provider) {
      nextKnown.set(key, { provider: ev.provider, model: ev.model });
    } else {
      const known = nextKnown.get(key);
      if (known) {
        ev.provider = known.provider;
        ev.model = known.model;
      } else {
        ev.provider = "unknown";
      }
    }
  }
}

/** Formatta il contatore escalation "n/max" quando entrambi noti. */
function formatAttempt(index?: number, max?: number): string | undefined {
  if (index == null) return undefined;
  return max != null ? `${index}/${max}` : String(index);
}

/** Etichette umane della causa STRUTTURATA di uno switch provider (vocabolario
 *  chiuso ProviderFailureCause del backend, regola M). PUNTO UNICO (regola L)
 *  riusato dalla banda "Cambio provider" del nastro e dalla card meta-step
 *  escalation: un rifiuto 4xx del provider o un'esclusione di policy NON vanno
 *  raccontati come cooldown. Cause ignote -> undefined (il renderer degrada al
 *  codice grezzo). */
export const SWITCH_CAUSE_LABELS: Record<string, string> = {
  cooldown: "provider in cooldown (indisponibilita' temporanea)",
  billing: "credito esaurito sul provider",
  client_error: "il provider ha rifiutato la richiesta (errore lato provider)",
  context_too_long: "richiesta troppo grande per il provider (passaggio a finestra piu' ampia)",
  policy_tier_excluded: "contenuto riservato: provider escluso dalla policy (sensitivity tier)",
};

/** Etichetta umana della causa, se nota. */
export function switchCauseLabel(cause?: string): string | undefined {
  return cause ? SWITCH_CAUSE_LABELS[cause] : undefined;
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
        // Conserva i ToolEvent originali per l'espansione (niente scarto).
        tools: run,
        // La provenienza viene dai passi compressi: senza, il blocco perdeva
        // provider e modello e il suo tooltip nominava un provider senza dire
        // su quale modello fossero girati i passi. I tool raccolti sono
        // consecutivi nello stesso segmento, quindi condividono la provenienza:
        // il primo la rappresenta tutta.
        provider: run[0].provider,
        model: run[0].model,
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

/**
 * Eventi che producono una riga. Gli `switch` non ne producono (diventano la
 * banda di intestazione del segmento), percio' restano fuori dal tipo: cosi' il
 * compilatore garantisce cio' che il raggruppamento gia' fa a runtime.
 */
export type EventoConRiga = Exclude<ActivityEvent, SwitchEvent>;

/** Una riga del nastro, con la sua posizione originale nel segmento. */
export interface RigaNastro {
  tipo: "riga";
  ev: EventoConRiga;
  indice: number;
}

/** Righe CONSECUTIVE dello stesso sub-run, raccolte per poterle collassare. */
export interface GruppoSubagente {
  tipo: "gruppo_subagente";
  subagentRunId: string;
  /**
   * Indice del primo evento del gruppo. Serve come chiave stabile: con il
   * dispatch parallelo gli eventi di sub-run diversi si interlacciano, quindi
   * lo stesso `subagentRunId` puo' aprire piu' gruppi distinti nel segmento e
   * l'id da solo non li distinguerebbe.
   */
  indice: number;
  eventi: RigaNastro[];
}

export type BloccoNastro = RigaNastro | GruppoSubagente;

/**
 * Divide gli eventi di un segmento in blocchi: le righe consecutive dello
 * stesso sub-agente formano un gruppo, tutto il resto resta una riga a se'.
 *
 * Raggruppa per CONSECUTIVITA' e non per sola chiave, cosi' l'ordine mostrato
 * resta quello reale degli eventi: se due sub-run procedono in parallelo e si
 * interlacciano, si formano piu' gruppi: mostrarli fusi mentirebbe sull'ordine.
 *
 * Gli eventi `switch` non producono una riga (il render li salta), percio' non
 * spezzano la continuita' di un gruppo: e' lo stesso criterio con cui il nastro
 * decideva gia' se ripetere l'intestazione del sub-agente.
 */
export function raggruppaBlocchiNastro(events: ActivityEvent[]): BloccoNastro[] {
  const out: BloccoNastro[] = [];
  events.forEach((ev, indice) => {
    if (ev.type === "switch") return;
    const riga: RigaNastro = { tipo: "riga", ev, indice };
    const id = ev.type === "subagent" ? ev.subagentRunId : undefined;
    if (!id) {
      out.push(riga);
      return;
    }
    const ultimo = out[out.length - 1];
    if (ultimo && ultimo.tipo === "gruppo_subagente" && ultimo.subagentRunId === id) {
      ultimo.eventi.push(riga);
      return;
    }
    out.push({ tipo: "gruppo_subagente", subagentRunId: id, indice, eventi: [riga] });
  });
  return out;
}

/**
 * Trace di un run (le trace in useChat sono per-sessione), INCLUSE quelle dei
 * suoi sub-run, a qualunque profondita'.
 *
 * Un subagente e' un run a se': le sue trace sono persistite sotto il PROPRIO
 * `run_id` (nexus_agent_traces), non sotto quello del padre. Filtrando per il
 * solo `runId` del padre, i token e il costo dei subagenti sparivano dal footer
 * costo-per-provider, e provider usati SOLO dal figlio non comparivano affatto.
 *
 * La parentela viene dal campo `parentRunId` che il backend annota su ogni
 * traccia di sub-run leggendolo dal DB (punto unico
 * `crates/mcp-core/src/run_lineage.rs`, regola L). Prima veniva dedotta dai
 * META-STEP di narrazione del padre: un canale di PRESENTAZIONE, che il review
 * panel non emette affatto. Da li' il difetto misurato il 26/07/2026 — la barra
 * dichiarava la ripartizione per provider di un run e ometteva i 4 cicli di
 * review su openrouter, che pure avevano girato 21 iterazioni. La misura
 * raggiungeva il suo oggetto per una strada diversa da quella della produzione
 * (regola O).
 *
 * La chiusura e' transitiva: un sub-run che ne convoca un altro porta con se'
 * anche i nipoti, che appartengono comunque al lavoro del run.
 */
export function tracesForRun(traces: AITraceEvent[], runId: string): AITraceEvent[] {
  const ids = new Set<string>([runId]);
  // Punto fisso: si aggiunge ogni run il cui padre e' gia' nell'insieme, finche'
  // l'insieme non smette di crescere. L'ordine delle trace non e' garantito
  // (arrivano raggruppate per run), quindi un solo passaggio perderebbe i nipoti
  // elencati prima dei genitori.
  let cresciuto = true;
  while (cresciuto) {
    cresciuto = false;
    for (const t of traces) {
      if (!t.parentRunId || ids.has(t.runId)) continue;
      if (ids.has(t.parentRunId)) {
        ids.add(t.runId);
        cresciuto = true;
      }
    }
  }
  return traces.filter((t) => ids.has(t.runId));
}

// ── Cap live (anti-verbosita') ──────────────────────────────────────────────
// Sui run lunghi il nastro LIVE diventa troppo lungo. Questa funzione pura
// restituisce una versione "cappata" dello stream che mostra solo gli ultimi
// `cap` eventi NON-switch, PRESERVANDO sempre tutte le bande "Cambio provider"
// (segmenti openedBySwitch: sono pochi e sono il segnale multi-provider chiave).
// L'evento piu' recente resta sempre visibile (in fondo). `hiddenCount` = numero
// di eventi non-switch nascosti (per il testo del toggle "Mostra tutti").
//
// Usato SOLO nel rendering live (AgentStepsPanel); lo storico mostra tutto.

export interface CappedStream {
  stream: ActivityStream;
  hiddenCount: number;
  /** totale eventi non-switch nello stream originale (per il testo "N eventi"). */
  totalEvents: number;
}

export function capStreamToRecent(stream: ActivityStream, cap: number): CappedStream {
  // Conta gli eventi non-switch totali (le bande switch non contano nel cap).
  const totalEvents = stream.segments.reduce(
    (n, seg) => n + seg.events.filter((e) => e.type !== "switch").length,
    0,
  );
  if (cap <= 0 || totalEvents <= cap) {
    return { stream, hiddenCount: 0, totalEvents };
  }

  // Soglia: teniamo solo gli ultimi `cap` eventi non-switch in ordine GLOBALE.
  // Calcoliamo l'indice globale del primo evento da tenere.
  const keepFromGlobalIndex = totalEvents - cap;

  let globalIndex = 0;
  const cappedSegments: ActivitySegment[] = [];
  for (const seg of stream.segments) {
    const keptEvents: ActivityEvent[] = [];
    let cappedInSeg = 0;
    for (const ev of seg.events) {
      if (ev.type === "switch") {
        // le bande switch vivono nel campo `seg.switch`, non negli events resi;
        // qui non incrementiamo il contatore (coerente con il conteggio sopra).
        keptEvents.push(ev);
        continue;
      }
      if (globalIndex >= keepFromGlobalIndex) keptEvents.push(ev);
      else cappedInSeg += 1;
      globalIndex += 1;
    }
    const hasVisibleEvent = keptEvents.some((e) => e.type !== "switch");
    // Un segmento resta se ha eventi visibili OPPURE ne ha di nascosti dal cap:
    // cosi' NESSUN provider che ha lavorato sparisce dalla vista live. Se ha solo
    // eventi cappati, il renderer mostra "N passi precedenti" (il dettaglio e'
    // nello storico). Prima il segmento veniva DROPPATO se non openedBySwitch ->
    // gli step di un provider intermedio (es. deepseek) sparivano, riapparendo
    // solo al refresh (che non cappa).
    if (hasVisibleEvent || cappedInSeg > 0 || seg.openedBySwitch) {
      // Lo spread `...seg` preserva `anchorId` del segmento; `keptEvents` sono i
      // ref evento ORIGINALI, quindi conservano il proprio `anchorId` (canonico,
      // pre-cap): il deep-link della campanella resta valido dopo il cap.
      cappedSegments.push({
        ...seg,
        events: keptEvents,
        cappedCount: cappedInSeg > 0 ? cappedInSeg : undefined,
      });
    }
  }

  return {
    stream: { segments: cappedSegments, empty: cappedSegments.length === 0 },
    hiddenCount: keepFromGlobalIndex,
    totalEvents,
  };
}

// ── Aggregazione costo-per-provider (regola G: prezzi dal catalogo) ──────────
// Aggrega SOLO i token per provider/model dalle trace del run. Il PREZZO viene
// applicato dal renderer col catalogo /api/models (useModelPricing di
// provider-badge.tsx): qui NON esiste alcun prezzo hardcoded.

export interface ProviderTokenBucket {
  provider: string;
  model: string;
  /** Token di prompt LORDI: comprendono i due conteggi di cache qui sotto. */
  inputTokens: number;
  outputTokens: number;
  /** Token serviti da cache: SOTTOINSIEME di `inputTokens`, con la sua tariffa.
   *  Senza questo campo il bucket non lo portava affatto e chi prezzava pagava
   *  tutto il prompt a tariffa piena di input. */
  cacheReadTokens: number;
  /** Token scritti in cache: stessa storia, tariffa ancora diversa. */
  cacheCreationTokens: number;
}

/** Una voce della ripartizione: il bucket con il suo costo in USD. */
export interface ProviderCostRow extends ProviderTokenBucket {
  costUsd: number;
}

/** Ripartizione per provider di un run: le voci, i token e il costo totali. */
export interface ProviderCostBreakdown {
  voci: ProviderCostRow[];
  totalTokens: number;
  totalCostUsd: number;
}

/**
 * Ripartizione costo-per-provider di un insieme di trace: PUNTO UNICO (regola L)
 * di cio' che la barra dei costi dichiara.
 *
 * Il prezzo arriva come funzione perche' il listino non e' qui (regola G: viene
 * dal catalogo `/api/models`); quello che vive qui e' la COMPOSIZIONE — quali
 * voci esistono e come si sommano — cioe' esattamente cio' che il difetto del
 * 26/07/2026 sbagliava. Le trace da passare sono quelle di `tracesForRun`, che
 * include i sub-run: una barra composta sulle sole trace del run padre dichiara
 * una ripartizione e ne omette i provider usati solo dai figli.
 */
export function providerCostBreakdown(
  traces: AITraceEvent[],
  prezzo: (bucket: ProviderTokenBucket) => number,
): ProviderCostBreakdown {
  const voci = aggregateTokensByProvider(traces).map((b) => ({ ...b, costUsd: prezzo(b) }));
  return {
    voci,
    // `inputTokens` e' il prompt LORDO: i token di cache sono gia' dentro, e
    // sommarli qui li conterebbe due volte.
    totalTokens: voci.reduce((s, v) => s + v.inputTokens + v.outputTokens, 0),
    totalCostUsd: voci.reduce((s, v) => s + v.costUsd, 0),
  };
}

/** Somma i token per coppia provider/model dalle trace del run, tenendo il
 *  DETTAGLIO di cache separato dal prompt lordo (ha tariffe diverse). */
export function aggregateTokensByProvider(traces: AITraceEvent[]): ProviderTokenBucket[] {
  const map = new Map<string, ProviderTokenBucket>();
  for (const t of traces) {
    const key = `${t.provider}|${t.model}`;
    const bucket = map.get(key) ?? {
      provider: t.provider,
      model: t.model,
      inputTokens: 0,
      outputTokens: 0,
      cacheReadTokens: 0,
      cacheCreationTokens: 0,
    };
    bucket.inputTokens += t.inputTokens ?? 0;
    bucket.outputTokens += t.outputTokens ?? 0;
    bucket.cacheReadTokens += t.cacheReadTokens ?? 0;
    bucket.cacheCreationTokens += t.cacheCreationTokens ?? 0;
    map.set(key, bucket);
  }
  return Array.from(map.values());
}

/** Ultimo segmento di un id modello (`x-ai/grok-4.5` -> `grok-4.5`): il prefisso
 *  nomina l'autore del modello, che nella voce e' gia' implicito nel provider. */
function nomeBreveModello(model: string): string {
  const i = model.lastIndexOf("/");
  return i >= 0 ? model.slice(i + 1) : model;
}

/** Etichette delle voci di costo, una per voce e nello stesso ordine.
 *
 *  Le voci sono aggregate per (provider, modello) perche' le tariffe cambiano
 *  col modello, ma l'etichetta mostrava il solo provider: quando lo stesso
 *  provider ha servito piu' modelli -- un failover, o un sub-run instradato su
 *  un modello leggero -- ne uscivano due righe identiche con importi diversi,
 *  che si leggono come un doppio conteggio (osservato il 29/07/2026: openrouter
 *  due volte, `x-ai/grok-4.5` e `z-ai/glm-4.7-flash`).
 *
 *  Il criterio e' che l'etichetta DISTINGUA la voce: il modello si aggiunge solo
 *  dove il provider da solo non basta, cosi' il caso comune resta corto in una
 *  barra che ha poco spazio. Un modello assente non produce etichetta piu'
 *  lunga di quella del provider: non avrebbe nulla da distinguere. */
export function etichetteVociCosto(voci: readonly ProviderTokenBucket[]): string[] {
  const vociPerProvider = new Map<string, number>();
  for (const v of voci) vociPerProvider.set(v.provider, (vociPerProvider.get(v.provider) ?? 0) + 1);
  return voci.map((v) => {
    const ambiguo = (vociPerProvider.get(v.provider) ?? 0) > 1;
    return ambiguo && v.model ? `${v.provider} ${nomeBreveModello(v.model)}` : v.provider;
  });
}
