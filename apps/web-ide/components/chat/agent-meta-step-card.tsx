"use client";

import { useState, type CSSProperties } from "react";
import { useEventOfKind } from "../../lib/project-dispatcher/hooks";
import { useThemeColors } from "../../lib/theme";
import { ProviderBadge, providerBaseColor } from "./provider-badge";
import { toolLabel } from "./tool-labels";
import { MarkdownBlock } from "./markdown-renderer";
import { switchCauseLabel } from "../../lib/use-chat/activity-stream";

/**
 * Card collassabile per visualizzare i meta-step semantici pubblicati dal
 * backend durante un run agente:
 *  - plan       → piano del planner_node (lista todo)
 *  - routing    → intent classificato + profilo + behavior_mode
 *  - clarify    → richiesta di chiarimento (Fase 2)
 *  - fallback   → cambio automatico di provider/modello su errore o cooldown
 *  - reflection → riassunto post-hoc del turno
 *
 * Tutti collassati di default tranne `clarify` (richiede risposta utente).
 *
 * NB stile: il web-ide NON usa Tailwind (nessuna dipendenza tailwindcss, nessun
 * file con @tailwind/@apply). Le classi utility erano percio' CSS morto: flex,
 * gap, colori e spaziature non venivano applicati e tutti i testi finivano
 * attaccati ("FallbackFallback", "delete_file/...errore"). Questo componente usa
 * quindi stili inline + `useThemeColors`, coerente col resto dell'IDE
 * (ProviderBadge, chat-panel, nexus-metrics-panel).
 */

type MetaStepKind = "plan" | "routing" | "clarify" | "fallback" | "reflection" | "next_actions" | string;

export interface AgentMetaStepData {
  kind: MetaStepKind;
  title: string;
  payload: Record<string, unknown>;
  correlationId?: string | null;
  createdAt: string;
}

// Scelta proposta dall'agente: testo breve del pulsante + prompt completo da inviare.
export type NextActionChoice = { label?: string; prompt?: string };

const NEXT_ACTION_ACCENT = "#3b82f6";

/** Converte un colore hex (#RRGGBB) in rgba con l'alpha dato. Usato per derivare
 *  sfondi/bordi tenui dal colore-accento del kind senza dipendere da Tailwind. */
function withAlpha(hex: string, alpha: number): string {
  const m = /^#?([0-9a-fA-F]{6})$/.exec(hex);
  if (!m) return hex;
  const v = m[1];
  const r = parseInt(v.slice(0, 2), 16);
  const g = parseInt(v.slice(2, 4), 16);
  const b = parseInt(v.slice(4, 6), 16);
  return `rgba(${r},${g},${b},${alpha})`;
}

/**
 * Estrae le scelte dell'ULTIMO meta_step `next_actions` di una timeline (gli
 * eventuali precedenti sono tentativi superati dai fallback). Ritorna [] se non
 * ci sono scelte. Punto unico per leggere le scelte (usato da chat-panel per
 * renderle a fine risposta).
 */
export function extractLatestNextActions(steps: AgentMetaStepData[]): NextActionChoice[] {
  let latest: NextActionChoice[] = [];
  for (const m of steps) {
    if (m.kind === "next_actions") {
      latest = (m.payload?.choices ?? []) as NextActionChoice[];
    }
  }
  return latest.filter((c) => c && (c.prompt ?? "").trim().length > 0);
}

/**
 * Invia la scelta selezionata: prop esplicita se fornita, altrimenti CustomEvent
 * globale `nexus:chat:send` (stesso bridge di "Risolvi con Nexus", ascoltato da
 * ide-shell). Punto unico per il comportamento di click-su-scelta.
 */
function dispatchChoice(prompt: string, onChoice?: (prompt: string) => void) {
  if (onChoice) {
    onChoice(prompt);
    return;
  }
  window.dispatchEvent(
    new CustomEvent("nexus:chat:send", { detail: { content: prompt, autoSend: true } }),
  );
}

/**
 * Pulsanti delle scelte di proseguimento. Punto unico di rendering (regola L):
 * usato sia a fine risposta in chat (posizione primaria voluta dall'utente) sia,
 * storicamente, dentro la card meta_step. Ogni voce, al click, invia subito in
 * chat il prompt gia' pronto (auto-send).
 *
 * Tastini pieni colorati affiancati (wrap su piu' righe se servono): riempimento
 * accent + testo bianco + ombra, chiaramente cliccabili a fine messaggio.
 */
export function NextActionsButtons({
  choices,
  onChoice,
}: {
  choices: NextActionChoice[];
  onChoice?: (prompt: string) => void;
}) {
  if (!choices.length) return null;
  return (
    <div style={{ display: "flex", flexDirection: "row", flexWrap: "wrap", gap: 8 }}>
      {choices.map((c, i) => {
        const label = c.label ?? "—";
        const prompt = c.prompt ?? "";
        return (
          <button
            key={i}
            type="button"
            title={prompt}
            disabled={!prompt}
            onClick={() => prompt && dispatchChoice(prompt, onChoice)}
            style={{
              display: "inline-flex",
              alignItems: "center",
              textAlign: "left",
              fontSize: 12,
              fontWeight: 600,
              padding: "6px 12px",
              borderRadius: 6,
              background: "#2563eb",
              color: "#fff",
              border: "none",
              boxShadow: "0 1px 2px rgba(0,0,0,0.15)",
              cursor: prompt ? "pointer" : "not-allowed",
              opacity: prompt ? 1 : 0.5,
            }}
          >
            {label}
          </button>
        );
      })}
    </div>
  );
}

interface KindDescriptor {
  icon: string;
  label: string;
  accent: string; // colore-accento (hex) per icona, label, bordo e sfondo tenue
  defaultOpen: boolean;
}

const KIND_MAP: Record<string, KindDescriptor> = {
  plan: { icon: "□", label: "Piano", accent: "#6366f1", defaultOpen: false },
  routing: { icon: "→", label: "Routing", accent: "#64748b", defaultOpen: false },
  clarify: { icon: "?", label: "Chiarimento", accent: "#f59e0b", defaultOpen: true },
  fallback: { icon: "↻", label: "Fallback", accent: "#f97316", defaultOpen: false },
  reflection: { icon: "◐", label: "Riflessione", accent: "#a855f7", defaultOpen: false },
  // Scelte proposte dall'agente: ogni voce diventa un pulsante a tutta larghezza.
  next_actions: { icon: "→", label: "Prossimi passi", accent: NEXT_ACTION_ACCENT, defaultOpen: true },
  // Ogni tool eseguito dall'executor emette questo meta_step (vedi
  // tool_dispatch del grafo). Icona "◆" distinta dal chevron "▸".
  tool_executed: { icon: "◆", label: "Tool", accent: "#10b981", defaultOpen: false },
  // Heartbeat "Sto interrogando <provider>/<model>" emesso dall'executor del grafo
  // (executor.rs kind=executor_call): la riga prende il colore del provider e il
  // nome provider/modello e' mostrato nel badge a destra.
  executor_call: { icon: "◇", label: "Modello", accent: "#64748b", defaultOpen: false },
  // Hard cap contesto (ADR 0016 D2): il run termina fail-fast quando la stima
  // token resta oltre il limite del modello anche dopo brake/upscale.
  context_overflow: { icon: "!", label: "Contesto oltre il limite", accent: "#ef4444", defaultOpen: true },
  // Fasi della narrazione live (emesse dal grafo nativo via emit_phase_meta):
  // promozione di modello (escalation/failover), interruzione anti-loop,
  // verifica oggettiva del final_gate, ciclo dichiarativo ADR 0034.
  escalation: { icon: "▲", label: "Cambio modello", accent: "#0ea5e9", defaultOpen: true },
  strategy_shift: { icon: "↷", label: "Cambio strategia", accent: "#8b5cf6", defaultOpen: true },
  loop_break: { icon: "◼", label: "Interruzione", accent: "#ef4444", defaultOpen: true },
  final_gate: { icon: "✓", label: "Verifica", accent: "#22c55e", defaultOpen: true },
  // ReviewGate del grafo (review_gate.rs): rimando in correzione su bocciatura,
  // chiusura bocciata al cap, o verdetto di chiusura (pass/inconclusive).
  review_gate: { icon: "▣", label: "Review", accent: "#f43f5e", defaultOpen: true },
  declaration_request: { icon: "…", label: "Richiesta esito", accent: "#f59e0b", defaultOpen: false },
  outcome_declared: { icon: "◆", label: "Esito dichiarato", accent: "#22c55e", defaultOpen: true },
  // Narrazione del sub-agente sul run padre (ponte subagent_native.rs, mig
  // 0535): avvio, progresso (tool inoltrati + heartbeat) e chiusura col
  // summary, correlati via correlation_id = subagent_run_id.
  subagent_started: { icon: "◈", label: "Subagente", accent: "#8b5cf6", defaultOpen: false },
  subagent_progress: { icon: "◈", label: "Subagente", accent: "#8b5cf6", defaultOpen: false },
  subagent_completed: { icon: "◈", label: "Subagente completato", accent: "#8b5cf6", defaultOpen: true },
  subagent_failed: { icon: "◈", label: "Subagente fallito", accent: "#ef4444", defaultOpen: true },
};

const DEFAULT_DESC: KindDescriptor = {
  icon: "•",
  label: "Step",
  accent: "#64748b",
  defaultOpen: false,
};

/** Meta-step TECNICI del canale SSE che non sono decisioni da mostrare come
 *  card: `usage_snapshot` (token del turno, consumato dalla barra contesto) ed
 *  `end_turn` (chiusura turno). Senza questo filtro comparivano come card
 *  "Step" senza titolo. Punto unico: usato da MessageMetaSteps (regola L). */
export const HIDDEN_META_KINDS: ReadonlySet<string> = new Set([
  "usage_snapshot",
  "end_turn",
  // Contabilita' tecnica dello stall-recovery (meta-reasoner): title vuoto,
  // payload solo session_id — non e' una decisione da mostrare come card.
  "stall_budget",
]);

/** Traduzioni delle chiavi di payload piu' comuni per la resa tabellare dei
 *  meta-step senza renderer dedicato. Le chiavi ignote degradano a
 *  snake_case -> parole ("max_cycles" -> "max cycles"), MAI a JSON grezzo. */
const PAYLOAD_KEY_LABELS: Record<string, string> = {
  cycle: "Tentativo",
  max_cycles: "Tentativi max",
  phase: "Fase",
  reason: "Motivo",
  attempt: "Tentativo",
  provider: "Provider",
  model: "Modello",
  iteration: "Iterazione",
  forced: "Chiusura forzata",
  failed_criteria: "Criteri falliti",
  outcome: "Esito",
  blocker: "Blocco",
  summary: "Sintesi",
  strategy: "Strategia",
  tools_count: "Tool disponibili",
  intent: "Intent",
  verdict: "Verdetto",
  findings: "Findings",
  valid: "Voti validi",
  total: "Review totali",
};

/** Valori del payload in forma leggibile: booleani "si'"/"no", primitivi as-is,
 *  strutture come stringa compatta (ultimo resort, mai blocchi JSON). */
function humanValue(v: unknown): string {
  if (v === null || v === undefined) return "—";
  if (typeof v === "boolean") return v ? "si'" : "no";
  if (typeof v === "string" || typeof v === "number") return String(v);
  return JSON.stringify(v);
}

/** Fasi note del final_gate in italiano (il title porta gia' il messaggio
 *  principale; qui il dettaglio del corpo espanso). */
const FINAL_GATE_PHASES: Record<string, string> = {
  start: "verifica avviata",
  passed: "superata",
  failed: "non superata, nuovo tentativo",
  forced_close: "chiusura al limite tentativi",
  completion_grace: "criteri ok, manca solo task_complete",
};

/** Etichette leggibili per i tipi di criterio del final_gate (payload
 * `failed_criteria[].type`, regola M: segnale strutturato, non prosa). */
const CRITERION_TYPE_LABELS: Record<string, string> = {
  run_command: "Comando di verifica",
  completion_confirmed: "Dichiarazione di chiusura",
  service_logs_clean: "Log servizio",
  http_endpoint: "Endpoint HTTP",
  design_verify: "Verifica visiva",
  no_orphan_outputs: "Output orfani",
  outputs_exist: "Output attesi",
  tool_capability: "Capacita' tool",
};

type FailedCriterionPayload = {
  type?: string;
  command?: string | null;
  excerpt?: string;
  exit_code?: number | null;
  build_errors?: number | null;
};

function criterionTypeLabel(t: string | undefined): string {
  if (!t) return "Criterio";
  return CRITERION_TYPE_LABELS[t] ?? t.replace(/_/g, " ");
}

function formatFailedCriterion(c: FailedCriterionPayload): string {
  const parts: string[] = [];
  if (c.command) parts.push(c.command);
  if (c.exit_code !== undefined && c.exit_code !== null) {
    parts.push(`exit ${c.exit_code}`);
  }
  if (typeof c.build_errors === "number" && c.build_errors > 0) {
    parts.push(`${c.build_errors} errori build`);
  }
  const head = parts.length > 0 ? parts.join(" · ") : null;
  const excerpt = (c.excerpt ?? "").trim();
  if (head && excerpt) return `${head} — ${excerpt}`;
  if (head) return head;
  if (excerpt) return excerpt;
  return "dettaglio non disponibile";
}

function FailedCriteriaList({
  items,
  tc,
}: {
  items: FailedCriterionPayload[];
  tc: ReturnType<typeof useThemeColors>;
}) {
  if (!items.length) return null;
  return (
    <div style={{ marginTop: 6 }}>
      <div style={{ fontSize: 11, fontWeight: 600, marginBottom: 4, color: tc.textMuted }}>
        Criteri non superati
      </div>
      <ul style={{ margin: 0, paddingLeft: 16, fontSize: 11, lineHeight: 1.45 }}>
        {items.map((c, i) => (
          <li key={`${c.type ?? "criterion"}-${i}`} style={{ marginBottom: 4 }}>
            <span style={{ fontWeight: 600, color: tc.text }}>
              {criterionTypeLabel(c.type)}
            </span>
            {": "}
            <span style={{ color: tc.textMuted }}>{formatFailedCriterion(c)}</span>
          </li>
        ))}
      </ul>
    </div>
  );
}

// M15.1 — Checklist todo del piano con aggiornamento LIVE via eventi TodoUpdated.
// Lo stato iniziale viene dal payload del meta_step plan; gli aggiornamenti
// arrivano in tempo reale (la checklist si spunta mentre l'agente lavora).
type PlanTodo = { id?: string; seq?: number; content?: string; status?: string; priority?: string };
// Esportata come punto unico della checklist plan live (regola L): riusata dal
// nastro attivita' (activity-stream.tsx) per non duplicare la logica di
// aggiornamento TodoUpdated via SSE.
export function PlanChecklist({ todos }: { todos: PlanTodo[] }) {
  // overrides[todo_id] = status piu' recente ricevuto via SSE.
  const [overrides, setOverrides] = useState<Record<string, string>>({});
  useEventOfKind(
    "TodoUpdated",
    (env) => {
      const p = env.payload as { todo_id: string; status: string };
      setOverrides((prev) => (prev[p.todo_id] === p.status ? prev : { ...prev, [p.todo_id]: p.status }));
    },
    [],
  );
  if (!todos.length) return <em style={{ fontSize: 11, opacity: 0.7 }}>Nessun todo</em>;
  const MARK: Record<string, string> = {
    completed: "[x]", in_progress: "[~]", blocked: "[!]", skipped: "[-]", pending: "[ ]",
  };
  return (
    <ol style={{ listStyle: "none", paddingLeft: 0, margin: 0, display: "flex", flexDirection: "column", gap: 2, fontSize: 12 }}>
      {todos.map((t, i) => {
        const status = (t.id ? overrides[t.id] : undefined) ?? t.status ?? "pending";
        return (
          <li key={t.id ?? i} style={{ lineHeight: 1.4, display: "flex", alignItems: "flex-start", gap: 6 }}>
            <span style={{ fontFamily: "var(--font-mono)", opacity: 0.7 }}>{MARK[status] ?? MARK.pending}</span>
            <span>
              {t.content ?? "—"}
              {t.priority && t.priority !== "normal" ? <span style={{ opacity: 0.6 }}> ({t.priority})</span> : null}
            </span>
          </li>
        );
      })}
    </ol>
  );
}

/** Riga chiave/valore per i payload tabellari (routing, fallback). */
function DefRow({ k, v, tc }: { k: string; v: string; tc: ReturnType<typeof useThemeColors> }) {
  return (
    <>
      <span style={{ opacity: 0.7, color: tc.textMuted }}>{k}</span>
      <span style={{ wordBreak: "break-word" }}>{v}</span>
    </>
  );
}

function renderPayload(
  kind: string,
  payload: Record<string, unknown>,
  onChoice: (prompt: string) => void,
  tc: ReturnType<typeof useThemeColors>,
) {
  if (kind === "next_actions") {
    const choices = (payload.choices ?? []) as NextActionChoice[];
    if (!choices.length) return <em style={{ fontSize: 11, opacity: 0.7 }}>Nessuna scelta</em>;
    return <NextActionsButtons choices={choices} onChoice={onChoice} />;
  }
  if (kind === "plan") {
    const todos = (payload.todos ?? []) as PlanTodo[];
    return <PlanChecklist todos={todos} />;
  }
  const grid: CSSProperties = {
    display: "grid",
    gridTemplateColumns: "auto 1fr",
    columnGap: 8,
    rowGap: 2,
    fontSize: 12,
  };
  if (kind === "routing") {
    const intent = String(payload.intent ?? "—");
    const profile = payload.profile_name as string | undefined;
    const mode = payload.behavior_mode as string | undefined;
    const budget = payload.token_budget as number | undefined;
    return (
      <div style={grid}>
        <DefRow k="Intent" v={intent} tc={tc} />
        {profile && <DefRow k="Profilo" v={profile} tc={tc} />}
        {mode && <DefRow k="Modalità" v={mode} tc={tc} />}
        {typeof budget === "number" && <DefRow k="Token budget" v={String(budget)} tc={tc} />}
      </div>
    );
  }
  if (kind === "fallback") {
    return (
      <div style={grid}>
        <DefRow k="A" v={`${String(payload.to_provider ?? "?")} / ${String(payload.to_model ?? "?")}`} tc={tc} />
        <DefRow k="Motivo" v={String(payload.reason ?? "—")} tc={tc} />
        <DefRow k="Tentativo" v={`#${String(payload.attempt ?? "?")}`} tc={tc} />
      </div>
    );
  }
  if (kind === "escalation") {
    // Cambio provider/modello: il motivo usa l'etichetta umana della causa
    // strutturata (punto unico switchCauseLabel, regola L/M) invece del codice
    // grezzo `provider_failover`, che resta il degrado per cause ignote.
    const cause = payload.cause as string | undefined;
    const reason = switchCauseLabel(cause) ?? String(payload.reason ?? cause ?? "—");
    return (
      <div style={grid}>
        <DefRow k="Da" v={`${String(payload.from_provider ?? "?")}`} tc={tc} />
        <DefRow k="A" v={`${String(payload.to_provider ?? "?")} / ${String(payload.to_model ?? "?")}`} tc={tc} />
        <DefRow k="Motivo" v={reason} tc={tc} />
      </div>
    );
  }
  if (kind === "clarify") {
    const question = String(payload.question ?? "");
    const rationale = payload.rationale as string | undefined;
    return (
      <div style={{ display: "flex", flexDirection: "column", gap: 4, fontSize: 12 }}>
        <p style={{ margin: 0, fontWeight: 500 }}>{question}</p>
        {rationale && <p style={{ margin: 0, opacity: 0.7, fontStyle: "italic" }}>{rationale}</p>}
      </div>
    );
  }
  if (kind === "reflection") {
    const summary = String(payload.summary ?? "");
    return (
      <div style={{ fontSize: 12, lineHeight: 1.4 }}>
        <MarkdownBlock content={summary} />
      </div>
    );
  }
  if (kind === "tool_executed") {
    const tool = String(payload.tool ?? "");
    const target = String(payload.target ?? "");
    const isErr = Boolean(payload.is_error);
    return (
      <div style={{ fontSize: 12, display: "flex", alignItems: "center", gap: 8, lineHeight: 1.4, flexWrap: "wrap" }}>
        <span style={{ fontWeight: 500, color: isErr ? tc.error : tc.text }}>{toolLabel(tool)}</span>
        {target && <span style={{ opacity: 0.7, wordBreak: "break-all" }}>{target}</span>}
        {isErr && <span style={{ color: tc.error, fontWeight: 600 }}>errore</span>}
      </div>
    );
  }
  if (kind === "executor_call") {
    // Heartbeat di interrogazione modello: metadati leggibili (provider/model sono
    // gia' nel badge a destra), NON il JSON grezzo. Il PENSIERO del modello arriva
    // nel ThinkingBlock via SseEvent::ThinkingDelta (emesso dall'executor).
    const intent = payload.intent as string | undefined;
    const iteration = payload.iteration as number | undefined;
    const toolsCount = payload.tools_count as number | undefined;
    return (
      <div style={grid}>
        {intent && <DefRow k="Intent" v={intent} tc={tc} />}
        {typeof iteration === "number" && <DefRow k="Iterazione" v={`#${iteration}`} tc={tc} />}
        {typeof toolsCount === "number" && <DefRow k="Tool disponibili" v={String(toolsCount)} tc={tc} />}
      </div>
    );
  }
  if (kind === "final_gate") {
    const cycle = payload.cycle as number | undefined;
    const maxCycles = payload.max_cycles as number | undefined;
    const phase = payload.phase as string | undefined;
    const failedRaw = payload.failed_criteria;
    const failedCriteria = Array.isArray(failedRaw)
      ? (failedRaw as FailedCriterionPayload[])
      : [];
    return (
      <div style={grid}>
        {typeof cycle === "number" && (
          <DefRow k="Tentativo" v={maxCycles ? `${cycle} di ${maxCycles}` : String(cycle)} tc={tc} />
        )}
        {phase && <DefRow k="Fase" v={FINAL_GATE_PHASES[phase] ?? phase} tc={tc} />}
        <FailedCriteriaList items={failedCriteria} tc={tc} />
      </div>
    );
  }
  // Fallback generico: coppie chiave/valore leggibili, MAI il JSON grezzo
  // (il payload e' telemetria strutturata; all'utente servono i fatti,
  // il dato raw resta disponibile agli sviluppatori via API/DB).
  const entries = Object.entries(payload ?? {}).filter(([, v]) => v !== null && v !== undefined);
  if (!entries.length) {
    return <em style={{ fontSize: 11, opacity: 0.7 }}>Nessun dettaglio aggiuntivo</em>;
  }
  return (
    <div style={grid}>
      {entries.map(([k, v]) => (
        <DefRow
          key={k}
          k={PAYLOAD_KEY_LABELS[k] ?? k.replace(/_/g, " ")}
          v={humanValue(v)}
          tc={tc}
        />
      ))}
    </div>
  );
}

export function AgentMetaStepCard({
  data,
  onChoice,
}: {
  data: AgentMetaStepData;
  /** Chiamata al click su una scelta `next_actions`. Se assente, fallback al
   *  bridge globale `nexus:chat:send` (stesso meccanismo di "Risolvi con Nexus"). */
  onChoice?: (prompt: string) => void;
}) {
  const tc = useThemeColors();
  const desc = KIND_MAP[data.kind] ?? DEFAULT_DESC;
  const [open, setOpen] = useState(desc.defaultOpen);

  // Invio della scelta: prop esplicita se fornita, altrimenti CustomEvent globale
  // ascoltato da ide-shell (imposta pendingChatMessage + pendingAutoSend).
  const handleChoice = (prompt: string) => {
    if (onChoice) {
      onChoice(prompt);
      return;
    }
    window.dispatchEvent(
      new CustomEvent("nexus:chat:send", { detail: { content: prompt, autoSend: true } }),
    );
  };

  // Provider/model per il turno (badge colorato per provider+costo).
  const provider = (data.payload?.provider ?? data.payload?.to_provider ?? null) as string | null;
  const model = (data.payload?.model ?? data.payload?.to_model ?? null) as string | null;

  // I meta_step tool arrivano col title gia' prefissato "tool <nome>": col label
  // "Tool" del descrittore diventerebbe "Tool tool <nome>". Rimuovo il prefisso
  // ridondante per i soli tool_executed.
  let displayTitle =
    data.kind === "tool_executed" ? data.title.replace(/^tool\s+/i, "") : data.title;
  // executor_call: il title backend e' "Sto interrogando <provider>/<model>"; il
  // provider/model finisce nel badge colorato a destra, quindi lo rimuovo dal testo
  // per non duplicarlo (resta "Sto interrogando").
  if (data.kind === "executor_call" && provider && model) {
    displayTitle = displayTitle.replace(`${provider}/${model}`, "").trim();
  }

  // Dedup label/title: se il title comincia gia' col label (es. label "Fallback"
  // + title "Fallback su google/...") mostrare entrambi dava "Fallback Fallback su
  // ..." (e senza Tailwind diventava "FallbackFallback"). In quel caso nascondo il
  // label e tengo solo il title, piu' descrittivo. Per i tool il label "Tool" e'
  // sempre ridondante col nome del tool, quindi mai mostrato.
  const titleLc = (displayTitle ?? "").trim().toLowerCase();
  const labelLc = desc.label.toLowerCase();
  const titleStartsWithLabel = titleLc.startsWith(labelLc);
  const showLabel = data.kind !== "tool_executed" && !titleStartsWithLabel;
  const titleToShow = titleLc && titleLc !== labelLc ? displayTitle : "";

  const showBadge =
    (provider || model) &&
    (data.kind === "routing" ||
      data.kind === "tool_executed" ||
      data.kind === "fallback" ||
      data.kind === "executor_call" ||
      // Narrazione sub-agente: il payload porta provider/model del FIGLIO
      // (arricchiti dal ponte, regola M) — mostrarli identifica chi lavora.
      data.kind.startsWith("subagent_"));

  // Colore-accento della riga: per gli step di chiamata modello usa il colore del
  // PROVIDER (ripristina il colore-per-provider richiesto in UI); altrimenti il
  // colore del kind.
  const accent =
    data.kind === "executor_call" && provider ? providerBaseColor(provider) : desc.accent;

  return (
    <div
      data-meta-step-kind={data.kind}
      style={{
        margin: "4px 0",
        borderRadius: 6,
        border: `1px solid ${withAlpha(accent, 0.3)}`,
        background: withAlpha(accent, 0.08),
        fontSize: 13,
      }}
    >
      <button
        type="button"
        onClick={() => setOpen((v) => !v)}
        aria-expanded={open}
        style={{
          width: "100%",
          display: "flex",
          alignItems: "center",
          gap: 8,
          padding: "5px 8px",
          background: "transparent",
          border: "none",
          color: accent,
          cursor: "pointer",
          textAlign: "left",
        }}
      >
        <span aria-hidden style={{ fontFamily: "var(--font-mono)" }}>{open ? "▾" : "▸"}</span>
        <span aria-hidden style={{ fontFamily: "var(--font-mono)" }}>{desc.icon}</span>
        {showLabel && <span style={{ fontWeight: 600 }}>{desc.label}</span>}
        {/* Title del turno: colore testo neutro (leggibile), troncato con ellipsis
            nell'header collassato; il dettaglio completo e' nel corpo espanso. */}
        <span
          style={{
            flex: 1,
            color: tc.text,
            opacity: 0.85,
            overflow: "hidden",
            textOverflow: "ellipsis",
            whiteSpace: "nowrap",
          }}
        >
          {titleToShow}
        </span>
        {showBadge && <ProviderBadge provider={provider} model={model} />}
      </button>
      {open && (
        <div style={{ padding: "0 12px 8px" }}>
          {renderPayload(data.kind, data.payload, handleChoice, tc)}
        </div>
      )}
    </div>
  );
}
