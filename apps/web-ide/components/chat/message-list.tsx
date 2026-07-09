"use client";

import { useEffect, useRef, useState, useCallback } from "react";
import type { RefObject } from "react";
import type { ChatMessage, AgentRunInfo, AgentStep, SavedChatAttachment, AITraceEvent } from "../../lib/api-client";
import { getAgentRun, getAgentRunNextActions, getAttachmentRawUrl } from "../../lib/api-client";
import type { useThemeColors } from "../../lib/theme";
import { MarkdownBlock } from "./markdown-renderer";
import { AgentMetaStepCard, HIDDEN_META_KINDS, NextActionsButtons, type NextActionChoice } from "./agent-meta-step-card";
import type { MetaStepEntry } from "../../lib/use-chat/types";
import { toolLabel } from "./tool-labels";
import {
  composeActivityStream,
  tracesForRun,
  type FoldThreshold,
} from "../../lib/use-chat/activity-stream";
import { ActivityStreamView } from "./activity-stream";
import { ActivityCostFooter } from "./activity-cost-footer";
import { ActivityHistoryRow } from "./activity-history-row";
import { InlineTruncated, formatStepInput } from "./step-detail";
import { useResolvedRunSteps } from "../../lib/use-chat/use-run-steps";

type ThemeColors = ReturnType<typeof useThemeColors>;

function parseThinking(content: string): { thinking: string | null; text: string } {
  const match = /^<nexus:thinking>([\s\S]*?)<\/nexus:thinking>\n*/s.exec(content);
  if (!match) return { thinking: null, text: content };
  return { thinking: match[1].trim(), text: content.slice(match[0].length) };
}

/** Estrae blocchi `tool_use` serializzati come JSON dal content di un
 * messaggio assistant. Pattern atteso: array JSON top-level con elementi
 * `{name, arguments?, input?}` — alcune codifiche brain mettono
 * `arguments`, altre `input`. Se l'intera content e' un array di tool_use,
 * `cleanText` resta vuoto e la UI mostra solo le pillole. Quando il content
 * mescola testo e tool_use, restituiamo cleanText senza il blocco JSON. */
type ToolUseBlock = { name: string; input: unknown };
function extractToolUseBlocks(content: string): { toolUses: ToolUseBlock[]; cleanText: string } {
  const trimmed = content.trim();
  if (!trimmed.startsWith("[") || !trimmed.endsWith("]")) {
    return { toolUses: [], cleanText: content };
  }
  try {
    const parsed = JSON.parse(trimmed);
    if (!Array.isArray(parsed)) return { toolUses: [], cleanText: content };
    const toolUses: ToolUseBlock[] = [];
    for (const b of parsed) {
      if (b && typeof b === "object" && typeof (b as Record<string, unknown>).name === "string") {
        toolUses.push({
          name: (b as Record<string, unknown>).name as string,
          input: (b as Record<string, unknown>).arguments ?? (b as Record<string, unknown>).input ?? {},
        });
      } else {
        // Array non omogeneo: non lo trattiamo come tool_use.
        return { toolUses: [], cleanText: content };
      }
    }
    return { toolUses, cleanText: "" };
  } catch {
    return { toolUses: [], cleanText: content };
  }
}

function summarizeToolInput(input: unknown): string {
  if (input == null || typeof input !== "object") return "";
  const entries = Object.entries(input as Record<string, unknown>);
  if (entries.length === 0) return "";
  const parts = entries.slice(0, 3).map(([k, v]) => {
    let val: string;
    if (typeof v === "string") val = v.length > 40 ? `"${v.slice(0, 40)}…"` : `"${v}"`;
    else if (typeof v === "number" || typeof v === "boolean") val = String(v);
    else if (v == null) val = "null";
    else val = "{…}";
    return `${k}=${val}`;
  });
  const extra = entries.length > 3 ? `, +${entries.length - 3}` : "";
  return parts.join(", ") + extra;
}

function ToolUseBadges({ toolUses, tc }: { toolUses: ToolUseBlock[]; tc: ThemeColors }) {
  return (
    <div style={{ display: "flex", flexDirection: "column", gap: 4, marginTop: 4 }}>
      {toolUses.map((tu, i) => {
        const summary = summarizeToolInput(tu.input);
        return (
          <div
            key={`tu-${i}`}
            style={{
              display: "inline-flex",
              alignItems: "baseline",
              gap: 6,
              padding: "3px 8px",
              borderRadius: 6,
              border: `1px solid ${tc.border}`,
              background: `${tc.bgInput}80`,
              fontSize: 11,
              fontFamily: "var(--font-mono)",
              maxWidth: "100%",
              overflow: "hidden",
              textOverflow: "ellipsis",
              whiteSpace: "nowrap",
              alignSelf: "flex-start",
            }}
            title={JSON.stringify(tu.input)}
          >
            <span style={{ opacity: 0.7 }}>⚙</span>
            <span style={{ fontWeight: 600 }}>{tu.name}</span>
            {summary && <span style={{ opacity: 0.8 }}>({summary})</span>}
          </div>
        );
      })}
    </div>
  );
}

function ThinkingPanel({ thinking }: { thinking: string }) {
  const [open, setOpen] = useState(false);
  return (
    <div style={{
      marginBottom: 8,
      borderRadius: 8,
      border: `1px solid #8b5cf644`,
      background: "#8b5cf608",
      overflow: "hidden",
    }}>
      <button
        type="button"
        onClick={() => setOpen((v) => !v)}
        style={{
          width: "100%",
          display: "flex",
          alignItems: "center",
          gap: 6,
          padding: "6px 10px",
          background: "transparent",
          border: "none",
          cursor: "pointer",
          color: "#8b5cf6",
          fontSize: 11,
          fontWeight: 600,
          textAlign: "left",
        }}
      >
        <span>{open ? "▾" : "▸"}</span>
        <span>Ragionamento interno</span>
        {!open && (
          <span style={{ color: "#8b5cf699", fontWeight: 400, fontSize: 10, marginLeft: 4 }}>
            ({Math.ceil(thinking.length / 5)} parole ca.)
          </span>
        )}
      </button>
      {open && (
        <div style={{
          padding: "8px 14px 12px",
          fontSize: 12,
          color: "#8b5cf6cc",
          wordBreak: "break-word",
          borderTop: "1px solid #8b5cf622",
          lineHeight: 1.6,
          maxHeight: 400,
          overflowY: "auto",
        }}>
          <MarkdownBlock content={thinking} />
        </div>
      )}
    </div>
  );
}

// Riconosce SOLO i messaggi-riepilogo sintetici legacy (formato "---" +
// "**Riepilogo run**"). Il vecchio criterio aggiuntivo `totalTokens > 0`
// marcava come riepilogo QUALSIASI risposta assistant con usage persistito
// (cioe' tutte, da quando i metadata token sono sempre scritti): appena due
// risposte erano consecutive nella lista visibile (run accodati/superseded,
// con gli user sintetici "Continua" filtrati), venivano collassate in
// "N run completati" e i loro contenuti diventavano inaccessibili in UI.
function isRunSummaryMessage(msg: ChatMessage): boolean {
  if (msg.role !== "assistant") return false;
  const trimmed = (msg.content ?? "").trim();
  return trimmed.startsWith("---") && trimmed.includes("**Riepilogo run**");
}

type GroupedItem =
  | { type: "message"; message: ChatMessage; idx: number }
  | { type: "run-group"; messages: ChatMessage[]; startIdx: number };

function groupMessages(messages: ChatMessage[]): GroupedItem[] {
  const result: GroupedItem[] = [];
  let i = 0;
  while (i < messages.length) {
    const msg = messages[i];
    if (isRunSummaryMessage(msg)) {
      const group: ChatMessage[] = [msg];
      let j = i + 1;
      while (j < messages.length && isRunSummaryMessage(messages[j])) {
        group.push(messages[j]);
        j++;
      }
      if (group.length > 1) {
        result.push({ type: "run-group", messages: group, startIdx: i });
        i = j;
      } else {
        result.push({ type: "message", message: msg, idx: i });
        i++;
      }
    } else {
      result.push({ type: "message", message: msg, idx: i });
      i++;
    }
  }
  return result;
}

function getRunInfo(msg: ChatMessage): { tokens: number; model: string } {
  if (msg.totalTokens && msg.totalTokens > 0) {
    const model = msg.provider && msg.model ? `${msg.provider}/${msg.model}` : msg.model ?? "";
    return { tokens: msg.totalTokens, model };
  }
  const tokenMatch = /(\d[\d\s]*)\s*token totali/.exec(msg.content ?? "");
  const tokens = tokenMatch ? parseInt(tokenMatch[1].replace(/\s/g, ""), 10) : 0;
  const modelMatch = /\(([^/]+\/[^)]+)\)/.exec(msg.content ?? "");
  const model = modelMatch ? modelMatch[1] : "";
  return { tokens, model };
}

function RunSummaryGroup({ messages, tc }: { messages: ChatMessage[]; tc: ThemeColors }) {
  const [expanded, setExpanded] = useState(false);
  const totalTokens = messages.reduce((sum, m) => {
    return sum + getRunInfo(m).tokens;
  }, 0);
  const lastModel = getRunInfo(messages[messages.length - 1]).model;

  return (
    <div
      style={{
        alignSelf: "flex-start",
        maxWidth: "96%",
        border: `1px solid ${tc.border}`,
        borderRadius: 8,
        background: tc.bgCard,
        overflow: "hidden",
      }}
    >
      <button
        type="button"
        onClick={() => setExpanded((v) => !v)}
        style={{
          width: "100%",
          display: "flex",
          alignItems: "center",
          gap: 8,
          padding: "6px 10px",
          background: "none",
          border: "none",
          cursor: "pointer",
          textAlign: "left",
          color: tc.textMuted,
          fontSize: 11,
        }}
      >
        <span style={{ fontSize: 10 }}>{expanded ? "▲" : "▼"}</span>
        <span style={{ color: tc.textSecondary, fontWeight: 600 }}>
          {messages.length} run completati
        </span>
        <span style={{ marginLeft: 4 }}>
          · {totalTokens.toLocaleString("it-IT")} token totali
          {lastModel && ` · ${lastModel}`}
        </span>
      </button>
      {expanded && (
        <div
          style={{
            borderTop: `1px solid ${tc.border}`,
            padding: "6px 10px",
            display: "flex",
            flexDirection: "column",
            gap: 8,
            maxHeight: 400,
            overflowY: "auto",
          }}
        >
          {messages.map((m, i) => {
            const { tokens, model } = getRunInfo(m);
            return (
              <div
                key={m.id}
                style={{
                  borderTop: i > 0 ? `1px solid ${tc.border}` : "none",
                  paddingTop: i > 0 ? 8 : 0,
                }}
              >
                <div style={{ fontSize: 11, color: tc.textMuted, display: "flex", gap: 6 }}>
                  <span style={{ color: tc.textSecondary }}>{tokens.toLocaleString("it-IT")} tok</span>
                  {model && <span>· {model}</span>}
                </div>
                {/* Contenuto integrale: prima l'espansione mostrava solo le
                    metriche e le risposte raggruppate restavano illeggibili. */}
                <div style={{ fontSize: 12, wordBreak: "break-word", overflowWrap: "anywhere" }}>
                  <MarkdownBlock content={m.content ?? ""} />
                </div>
              </div>
            );
          })}
        </div>
      )}
    </div>
  );
}

// ── Pannello step agente inline (caricamento lazy da DB via runId) ──

function StepStatusBadge({ status, tc }: { status: AgentStep["status"]; tc: ThemeColors }) {
  const config: Record<string, { label: string; color: string; bg: string }> = {
    completed: { label: "ok", color: "#22c55e", bg: "#22c55e18" },
    failed: { label: "errore", color: tc.error, bg: `${tc.error}18` },
    running: { label: "in corso", color: tc.accent, bg: `${tc.accent}18` },
    skipped: { label: "saltato", color: tc.textMuted, bg: `${tc.border}20` },
    provider_unavailable: { label: "provider n/d", color: "#f59e0b", bg: "#f59e0b18" },
    awaiting_confirmation: { label: "in attesa", color: "#8b5cf6", bg: "#8b5cf618" },
  };
  const c = config[status] ?? config.running;
  return (
    <span style={{
      fontSize: 10,
      fontWeight: 600,
      color: c.color,
      background: c.bg,
      borderRadius: 4,
      padding: "1px 5px",
      whiteSpace: "nowrap",
    }}>
      {c.label}
    </span>
  );
}

// Badge PERSISTENTE dello stato del turno, mostrato sotto la risposta assistant.
// Lo stato e' quello canonico del run (agent_runs.status via backend), quindi
// resta coerente anche dopo un reload e rende esplicito "cosa e' successo e
// com'e' finito" — chiude la percezione di turni scollegati.
function RunStatusBadge({ status, tc }: { status: string; tc: ThemeColors }) {
  const config: Record<string, { label: string; color: string; bg: string }> = {
    completed: { label: "completato", color: "#22c55e", bg: "#22c55e18" },
    completed_verified: { label: "completato e verificato", color: "#22c55e", bg: "#22c55e18" },
    completed_unverified: { label: "completato (non verificato)", color: "#f59e0b", bg: "#f59e0b18" },
    failed: { label: "non riuscito", color: tc.error, bg: `${tc.error}18` },
    failed_diagnosed: { label: "non riuscito (diagnosi)", color: tc.error, bg: `${tc.error}18` },
    timed_out: { label: "tempo scaduto", color: tc.error, bg: `${tc.error}18` },
    loop_aborted: { label: "interrotto (loop)", color: tc.error, bg: `${tc.error}18` },
    cancelled: { label: "interrotto", color: tc.textMuted, bg: `${tc.border}20` },
    interrupted: { label: "interrotto (riavvio)", color: "#f59e0b", bg: "#f59e0b18" },
    provider_unavailable: { label: "provider non disponibile", color: "#f59e0b", bg: "#f59e0b18" },
    awaiting_confirmation: { label: "in attesa di conferma", color: "#8b5cf6", bg: "#8b5cf618" },
    awaiting_subagents: { label: "in attesa dei sub-agent", color: "#8b5cf6", bg: "#8b5cf618" },
    blocked_needs_input: { label: "in attesa di input", color: "#8b5cf6", bg: "#8b5cf618" },
    running: { label: "in corso", color: tc.accent, bg: `${tc.accent}18` },
  };
  const c = config[status];
  if (!c) return null;
  return (
    <span style={{
      display: "inline-flex",
      alignItems: "center",
      fontSize: 10,
      fontWeight: 600,
      color: c.color,
      background: c.bg,
      border: `1px solid ${c.color}40`,
      borderRadius: 5,
      padding: "1px 7px",
      whiteSpace: "nowrap",
    }}>
      {c.label}
    </span>
  );
}

/** Esito di un risveglio automatico di sistema, ricostruito dal messaggio
 *  sintetico iniettato dal worker backend process_resume. */
type SystemWakeup = { outcome: "success" | "failure" | "cap"; label: string };

/**
 * Riconosce se un messaggio e' un RISVEGLIO AUTOMATICO di sistema (worker
 * `process_resume`, crates/mcp-core/src/process_resume.rs): l'agente viene
 * ri-avviato da solo quando un processo/servizio del progetto termina o va in
 * crash, senza che l'utente lo abbia chiesto.
 *
 * NOTA (ripiego dichiarato, regole M/H): il backend marca questi messaggi con
 * `metadata.source = "process_resume"`, ma `to_message_view`
 * (crates/mcp-core/src/chat_messages/persistence.rs) NON espone ancora il campo
 * `source` alla UI. Finche' non viene esposto un marcatore STRUTTURATO,
 * riconosciamo il risveglio dal TESTO del messaggio sintetico (fragile per
 * definizione). Appena il backend espone `source`/`syntheticKind`, questa
 * detection va spostata su quel campo (vedi report/handoff backend).
 */
function classifySystemWakeup(message: ChatMessage): SystemWakeup | null {
  if (!message.synthetic || message.role !== "user") return null;
  const content = message.content ?? "";
  let m = /^Il comando in background "([^"]+)" e' terminato con SUCCESSO/.exec(content);
  if (m) return { outcome: "success", label: m[1] };
  m = /^Il comando in background "([^"]+)" e' FALLITO/.exec(content);
  if (m) return { outcome: "failure", label: m[1] };
  m = /^Cap anti-loop raggiunto: il processo di sfondo "([^"]+)"/.exec(content);
  if (m) return { outcome: "cap", label: m[1] };
  return null;
}

// Banner distintivo per un turno nato da un risveglio automatico di sistema.
// Rende esplicito all'utente che l'agente si e' svegliato da solo (consuma token
// e tocca i file del progetto) in reazione a un evento, non su sua richiesta.
function SystemWakeupBanner({
  wakeup,
  tc,
  t,
}: {
  wakeup: SystemWakeup;
  tc: ThemeColors;
  t: (key: string) => string;
}) {
  const accent = "#f59e0b";
  const outcomeKey =
    wakeup.outcome === "success"
      ? "chat.systemWakeup.success"
      : wakeup.outcome === "failure"
        ? "chat.systemWakeup.failure"
        : "chat.systemWakeup.cap";
  return (
    <div
      style={{
        alignSelf: "stretch",
        display: "flex",
        flexDirection: "column",
        gap: 4,
        padding: "8px 10px",
        borderRadius: 10,
        border: `1px solid ${accent}55`,
        background: `${accent}12`,
        minWidth: 0,
        overflow: "hidden",
      }}
    >
      <div style={{ display: "flex", alignItems: "center", gap: 8, minWidth: 0 }}>
        <span
          style={{
            flexShrink: 0,
            display: "inline-flex",
            alignItems: "center",
            fontSize: 10,
            fontWeight: 700,
            letterSpacing: 0.2,
            color: accent,
            background: `${accent}1f`,
            border: `1px solid ${accent}66`,
            borderRadius: 5,
            padding: "1px 7px",
            whiteSpace: "nowrap",
          }}
        >
          {t("chat.systemWakeup.badge")}
        </span>
        <span
          style={{
            flex: 1,
            minWidth: 0,
            overflow: "hidden",
            textOverflow: "ellipsis",
            whiteSpace: "nowrap",
            fontSize: 11,
            color: tc.textSecondary,
          }}
          title={wakeup.label}
        >
          {t(outcomeKey)}
          {wakeup.label ? " — " : ""}
          {wakeup.label ? <code style={{ fontSize: 10, color: tc.textMuted }}>{wakeup.label}</code> : null}
        </span>
      </div>
      <span style={{ fontSize: 10, color: tc.textMuted, lineHeight: 1.35 }}>
        {t("chat.systemWakeup.explain")}
      </span>
    </div>
  );
}


/**
 * Pulsanti delle scelte di proseguimento (next_actions) per un singolo messaggio
 * assistant. Sorgente robusta a due livelli:
 *   1. `liveChoices` (fast-path): scelte arrivate via SSE nel run corrente,
 *      mostrate subito senza attendere il fetch.
 *   2. fallback DB: rilegge le scelte persistite (nexus_agent_meta_steps) tramite
 *      l'endpoint dedicato, cosi' i pulsanti RESTANO anche dopo un reload o sui
 *      turni passati (i dati SSE si perdono al refresh). Una sola fetch per runId.
 * Best-effort: in caso di errore/nessuna scelta non rende nulla.
 */
function MessageNextActions({
  runId,
  liveChoices,
  tc,
}: {
  runId: string;
  liveChoices?: NextActionChoice[];
  tc: ThemeColors;
}) {
  const [choices, setChoices] = useState<NextActionChoice[]>(liveChoices ?? []);

  // Fast-path live: se arrivano scelte via SSE le adottiamo subito.
  useEffect(() => {
    if (liveChoices && liveChoices.length) setChoices(liveChoices);
  }, [liveChoices]);

  // Fallback DB: una sola fetch per runId; non sovrascrive scelte gia' presenti.
  useEffect(() => {
    let alive = true;
    getAgentRunNextActions(runId)
      .then((r) => {
        const fromDb = (r.choices ?? []) as NextActionChoice[];
        if (alive && fromDb.length) {
          setChoices((prev) => (prev.length ? prev : fromDb));
        }
      })
      .catch(() => {
        /* best-effort: senza scelte non mostriamo pulsanti */
      });
    return () => {
      alive = false;
    };
  }, [runId]);

  if (!choices.length) return null;
  return (
    <div style={{ marginTop: 8 }} data-testid="chat-next-actions">
      <div style={{ fontSize: 11, fontWeight: 600, color: tc.textMuted, marginBottom: 4 }}>
        Scegli come proseguire
      </div>
      <NextActionsButtons choices={choices} />
    </div>
  );
}

/**
 * Card "decisioni del turno" (meta_step plan/routing/clarify/fallback/
 * reflection/tool_executed) per UN messaggio assistant (FIX D6). Prima esisteva
 * un unico blocco in chat-panel per il solo ultimo run; ora ogni messaggio
 * assistant con runId mostra le sue decisioni, cosi' i turni passati restano
 * leggibili dopo un reload (convergenza live/refresh, regola L). I next_actions
 * sono esclusi (resi come pulsanti da MessageNextActions). Best-effort: se non
 * ci sono decisioni non rende nulla.
 */
function MessageMetaSteps({ steps, tc }: { steps: MetaStepEntry[]; tc: ThemeColors }) {
  // Esclusi i next_actions (resi come pulsanti) e i kind TECNICI del canale
  // SSE (usage_snapshot/end_turn): comparivano come card "Step" senza titolo.
  const decisionSteps = steps.filter(
    (m) => m.kind !== "next_actions" && !HIDDEN_META_KINDS.has(m.kind),
  );
  if (!decisionSteps.length) return null;
  return (
    <div
      style={{
        marginTop: 6,
        border: `1px solid ${tc.border}`,
        borderRadius: 8,
        background: tc.bgCard,
        padding: "6px 10px",
      }}
      data-testid="message-meta-steps"
    >
      <div style={{ fontSize: 11, fontWeight: 600, color: tc.textMuted, marginBottom: 4 }}>
        Decisioni del turno
      </div>
      {decisionSteps.map((m, idx) => (
        <AgentMetaStepCard key={`meta-${m.kind}-${m.createdAt}-${idx}`} data={m} />
      ))}
    </div>
  );
}

/**
 * Nastro attivita' per UN messaggio assistant con runId (ADR 0037). Compone lo
 * stream dal punto unico (metaSteps + steps + traces del run) e lo rende come
 * nastro + footer costo-per-provider. Reso SOLO col flag activity_stream_enabled
 * ON, al posto di MessageMetaSteps/AgentRunStepsInline. Best-effort: se non c'e'
 * alcun segnale non rende nulla.
 */
function MessageActivityStream({
  runId,
  metaSteps,
  steps,
  traces,
  foldThreshold,
  tc,
}: {
  runId: string;
  metaSteps: MetaStepEntry[];
  steps: AgentStep[];
  traces: AITraceEvent[];
  foldThreshold: FoldThreshold;
  tc: ThemeColors;
}) {
  // Lazy-fetch degli step storici quando mancano (agentStepsMap non popolato al
  // bootstrap per i run passati): senza, il nastro storico mostrerebbe le
  // decisioni ma NON i tool. Se gli step sono gia' presenti, niente fetch.
  const resolvedSteps = useResolvedRunSteps(runId, steps);
  const stream = composeActivityStream(metaSteps, resolvedSteps, traces, foldThreshold);
  if (stream.empty) return null;
  return (
    <div style={{ minWidth: 0 }}>
      <ActivityStreamView stream={stream} tc={tc} />
      {traces.length > 0 && <ActivityCostFooter traces={traces} tc={tc} />}
    </div>
  );
}

/** Raggruppa step consecutivi con stesso toolName e stesso status (esclude
 *  supervisor_check, sempre singolo). Stessa logica del pannello live
 *  (agent-steps-panel SingleRunPanel) per la parita' di rendering storico/live
 *  (FIX D1). Ritorna gruppi con il count e il range di indici originali. */
type StepGroup = { step: AgentStep; count: number; firstIndex: number; lastIndex: number };
function groupConsecutiveSteps(steps: AgentStep[]): StepGroup[] {
  const groups: StepGroup[] = [];
  for (const step of steps) {
    const last = groups[groups.length - 1];
    if (
      last &&
      last.step.toolName === step.toolName &&
      last.step.status === step.status &&
      step.toolName !== "supervisor_check"
    ) {
      last.count += 1;
      last.lastIndex = step.stepIndex;
      last.step = step;
    } else {
      groups.push({ step, count: 1, firstIndex: step.stepIndex, lastIndex: step.stepIndex });
    }
  }
  return groups;
}

function AgentRunStepsInline({ runId, tc }: { runId: string; tc: ThemeColors }) {
  const [open, setOpen] = useState(false);
  const [loading, setLoading] = useState(false);
  const [runInfo, setRunInfo] = useState<AgentRunInfo | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [expandedIdx, setExpandedIdx] = useState<number | null>(null);

  const load = useCallback(async () => {
    if (runInfo) { setOpen(v => !v); return; }
    setLoading(true);
    setError(null);
    try {
      const info = await getAgentRun(runId);
      setRunInfo(info);
      setOpen(true);
    } catch (e) {
      setError(e instanceof Error ? e.message : "Errore nel caricamento");
    } finally {
      setLoading(false);
    }
  }, [runId, runInfo]);

  const steps = runInfo?.steps ?? [];

  return (
    <div style={{ marginTop: 6 }}>
      <button
        type="button"
        onClick={load}
        disabled={loading}
        style={{
          display: "inline-flex",
          alignItems: "center",
          gap: 5,
          fontSize: 11,
          fontWeight: 600,
          color: tc.accent,
          background: `${tc.accent}10`,
          border: `1px solid ${tc.accent}30`,
          borderRadius: 6,
          padding: "3px 10px",
          cursor: loading ? "wait" : "pointer",
          transition: "background 0.15s",
        }}
      >
        <span style={{ fontSize: 10 }}>{open ? "▼" : "▶"}</span>
        {loading
          ? "Caricamento step..."
          : runInfo
            ? `${open ? "Nascondi" : "Mostra"} ${steps.length} step eseguiti`
            : "Mostra step agente"}
      </button>

      {error && (
        <div style={{ fontSize: 11, color: tc.error, marginTop: 4 }}>{error}</div>
      )}

      {open && runInfo && steps.length > 0 && (
        <div style={{
          marginTop: 6,
          border: `1px solid ${tc.border}`,
          borderRadius: 8,
          background: `${tc.bgCard}`,
          overflow: "hidden",
        }}>
          {/* Header riepilogo run (parita' col live, FIX D1): provider/model +
              badge di stato persistente + conteggi. */}
          <div style={{
            padding: "6px 10px",
            borderBottom: `1px solid ${tc.border}`,
            display: "flex",
            alignItems: "center",
            gap: 8,
            fontSize: 11,
            color: tc.textMuted,
            flexWrap: "wrap",
          }}>
            <span style={{ fontWeight: 600, color: tc.text }}>
              {runInfo.provider}/{runInfo.model}
            </span>
            <RunStatusBadge status={runInfo.status} tc={tc} />
            <span>{steps.length} step</span>
            {runInfo.iterationCount > 0 && <span>{runInfo.iterationCount} iterazioni</span>}
          </div>

          {/* Lista step con raggruppamento consecutivo xN (parita' col live):
              step adiacenti con stesso toolName e stesso status sono collassati
              in una riga con badge x{count} e range N-M. */}
          <div style={{ padding: "4px 0" }}>
            {groupConsecutiveSteps(steps).map((g) => {
              const step = g.step;
              const isExp = expandedIdx === step.stepIndex;
              const hasInput = step.toolInput && Object.keys(step.toolInput).length > 0;
              const hasResult = Boolean(step.toolResult);
              const clickable = hasInput || hasResult;
              const borderColor =
                step.status === "failed" ? tc.error :
                step.status === "running" ? tc.accent : "#22c55e";

              return (
                <div key={`${step.stepIndex}-${g.count}`}>
                  {/* Riga step */}
                  <div
                    onClick={() => clickable && setExpandedIdx(isExp ? null : step.stepIndex)}
                    style={{
                      display: "flex",
                      alignItems: "center",
                      gap: 6,
                      padding: "4px 10px",
                      cursor: clickable ? "pointer" : "default",
                      background: isExp ? `${tc.border}10` : "transparent",
                      transition: "background 0.12s",
                    }}
                    onMouseEnter={(e) => { if (clickable && e.currentTarget instanceof HTMLElement) e.currentTarget.style.background = `${tc.border}18`; }}
                    onMouseLeave={(e) => { if (clickable && e.currentTarget instanceof HTMLElement) e.currentTarget.style.background = isExp ? `${tc.border}10` : "transparent"; }}
                  >
                    <span style={{ minWidth: 28, textAlign: "right", opacity: 0.5, fontSize: 10, fontVariantNumeric: "tabular-nums" }}>
                      {g.count > 1 ? `${g.firstIndex + 1}–${g.lastIndex + 1}` : `${g.firstIndex + 1}.`}
                    </span>
                    {clickable && (
                      <span style={{ fontSize: 9, opacity: 0.5, minWidth: 10 }}>
                        {isExp ? "▼" : "▶"}
                      </span>
                    )}
                    <span style={{ fontSize: 11, color: tc.text }}>
                      {toolLabel(step.toolName)}
                    </span>
                    <StepStatusBadge status={step.status} tc={tc} />
                    {g.count > 1 && (
                      <span style={{
                        marginLeft: 2,
                        background: step.status === "failed" ? `${tc.error}22` : "#22c55e22",
                        color: step.status === "failed" ? tc.error : "#22c55e",
                        border: `1px solid ${step.status === "failed" ? tc.error : "#22c55e"}44`,
                        borderRadius: 10,
                        padding: "0px 6px",
                        fontSize: 10,
                        fontWeight: 700,
                        fontVariantNumeric: "tabular-nums",
                        lineHeight: "16px",
                      }}>
                        x{g.count}
                      </span>
                    )}
                  </div>

                  {/* Dettaglio espanso */}
                  {isExp && (
                    <div style={{
                      marginLeft: 32,
                      marginRight: 10,
                      marginBottom: 6,
                      paddingLeft: 8,
                      paddingTop: 4,
                      paddingBottom: 4,
                      borderLeft: `2px solid ${borderColor}40`,
                      display: "flex",
                      flexDirection: "column",
                      gap: 6,
                    }}>
                      {hasInput && (
                        <div>
                          <div style={{ fontWeight: 600, fontSize: 10, textTransform: "uppercase", letterSpacing: "0.05em", opacity: 0.6, marginBottom: 3 }}>
                            Parametri
                          </div>
                          <InlineTruncated text={formatStepInput(step.toolInput)} tc={tc} />
                        </div>
                      )}
                      {hasResult && (
                        <div>
                          <div style={{
                            fontWeight: 600,
                            fontSize: 10,
                            textTransform: "uppercase",
                            letterSpacing: "0.05em",
                            opacity: 0.6,
                            marginBottom: 3,
                            color: step.status === "failed" ? tc.error : undefined,
                          }}>
                            {step.status === "failed" ? "Errore" : "Risultato"}
                          </div>
                          <InlineTruncated text={step.toolResult!} maxLen={500} tc={tc} />
                        </div>
                      )}
                      {step.createdAt && (
                        <div style={{ fontSize: 10, opacity: 0.5, fontFamily: "var(--font-mono)" }}>
                          {new Date(step.createdAt).toLocaleTimeString()}
                        </div>
                      )}
                    </div>
                  )}
                </div>
              );
            })}
          </div>

          {/* Footer esito (parita' col live): "Completato/Fallito - N step". */}
          {(runInfo.status === "completed" || runInfo.status === "completed_verified" ||
            runInfo.status === "completed_unverified" ||
            runInfo.status === "failed" || runInfo.status === "failed_diagnosed") && (
            <div style={{
              padding: "5px 10px",
              borderTop: `1px solid ${tc.border}`,
              fontSize: 11,
              fontWeight: 600,
              color: (runInfo.status === "failed" || runInfo.status === "failed_diagnosed")
                ? tc.error
                : runInfo.status === "completed_unverified"
                  ? "#f59e0b"
                  : "#22c55e",
            }}>
              {(runInfo.status === "failed" || runInfo.status === "failed_diagnosed")
                ? "✗ Fallito"
                : runInfo.status === "completed_unverified"
                  ? "✓ Completato (verifica non eseguita)"
                  : "✓ Completato"} — {steps.length} step
            </div>
          )}
        </div>
      )}

      {open && runInfo && steps.length === 0 && (
        <div style={{ fontSize: 11, color: tc.textMuted, marginTop: 4, fontStyle: "italic" }}>
          Nessuno step registrato per questo run.
        </div>
      )}
    </div>
  );
}

/** Formattazione dimensione in stringa human-readable. */
function formatBytes(size: number): string {
  if (!Number.isFinite(size) || size <= 0) return "0 B";
  if (size < 1024) return `${size} B`;
  if (size < 1024 * 1024) return `${(size / 1024).toFixed(1)} KB`;
  return `${(size / (1024 * 1024)).toFixed(1)} MB`;
}

/** Riga di chip per gli allegati persistiti di un messaggio chat.
 *  - immagini: thumbnail + click apre raw URL in nuova tab
 *  - testo/binario: click dispatchca `nexus:editor:open-file` con il path
 *  - se `indexedAt` valorizzato, lo chip mostra un badge KB verde. */
function AttachmentChips({
  attachments,
  tc,
}: {
  attachments: SavedChatAttachment[];
  tc: ThemeColors;
}) {
  if (!attachments?.length) return null;

  const openInEditor = (path: string) => {
    if (typeof window === "undefined") return;
    window.dispatchEvent(
      new CustomEvent("nexus:editor:open-file", { detail: { path } }),
    );
  };

  return (
    <div
      style={{
        display: "flex",
        flexWrap: "wrap",
        gap: 6,
        marginTop: 8,
        paddingTop: 6,
        borderTop: `1px dashed ${tc.border}`,
      }}
    >
      {attachments.map((att) => {
        const isIndexed = Boolean(att.indexedAt);
        const isImage = att.kind === "image";
        const rawUrl = getAttachmentRawUrl(att.id);
        const baseStyle: React.CSSProperties = {
          display: "inline-flex",
          alignItems: "center",
          gap: 6,
          padding: "3px 8px",
          borderRadius: 8,
          border: `1px solid ${isIndexed ? tc.success + "88" : tc.border}`,
          background: isIndexed ? tc.success + "1f" : tc.bgInput,
          color: tc.text,
          fontSize: 11,
          fontFamily: "inherit",
          cursor: "pointer",
          maxWidth: 240,
          overflow: "hidden",
          textOverflow: "ellipsis",
          whiteSpace: "nowrap",
        };
        const title = isIndexed
          ? `${att.fileName} (${formatBytes(att.sizeBytes)}) — indicizzato in KB`
          : `${att.fileName} (${formatBytes(att.sizeBytes)})`;

        if (isImage) {
          return (
            <a
              key={att.id}
              href={rawUrl}
              target="_blank"
              rel="noreferrer noopener"
              title={title}
              style={{ ...baseStyle, textDecoration: "none" }}
            >
              <span aria-hidden style={{ fontSize: 10, fontWeight: 700, color: tc.textSecondary, letterSpacing: "0.5px" }}>IMG</span>
              <span style={{ overflow: "hidden", textOverflow: "ellipsis" }}>{att.fileName}</span>
              <span style={{ color: tc.textMuted, fontSize: 10 }}>
                {formatBytes(att.sizeBytes)}
              </span>
              {isIndexed && (
                <span
                  aria-label="Indicizzato in Knowledge Base"
                  title="Indicizzato in Knowledge Base"
                  style={{ color: tc.success, fontWeight: 700, fontSize: 10 }}
                >
                  ⌘ KB
                </span>
              )}
            </a>
          );
        }

        // Per i binari (formati non leggibili come testo) usiamo un link al
        // raw URL che il browser scarica via Content-Disposition: attachment.
        // Per i testi apriamo nell'editor via l'evento globale.
        if (att.kind === "binary") {
          return (
            <a
              key={att.id}
              href={rawUrl}
              target="_blank"
              rel="noreferrer noopener"
              download={att.fileName}
              title={title}
              style={{ ...baseStyle, textDecoration: "none" }}
            >
              <span aria-hidden style={{ fontSize: 10, fontWeight: 700, color: tc.textSecondary, letterSpacing: "0.5px" }}>BIN</span>
              <span style={{ overflow: "hidden", textOverflow: "ellipsis" }}>{att.fileName}</span>
              <span style={{ color: tc.textMuted, fontSize: 10 }}>
                {formatBytes(att.sizeBytes)}
              </span>
              {isIndexed && (
                <span
                  aria-label="Indicizzato in Knowledge Base"
                  title="Indicizzato in Knowledge Base"
                  style={{ color: tc.success, fontWeight: 700, fontSize: 10 }}
                >
                  ⌘ KB
                </span>
              )}
            </a>
          );
        }

        return (
          <button
            key={att.id}
            type="button"
            onClick={() => openInEditor(att.filePath)}
            title={title}
            style={baseStyle}
          >
            <span aria-hidden style={{ fontSize: 10, fontWeight: 700, color: tc.textSecondary, letterSpacing: "0.5px" }}>TXT</span>
            <span style={{ overflow: "hidden", textOverflow: "ellipsis" }}>{att.fileName}</span>
            <span style={{ color: tc.textMuted, fontSize: 10 }}>
              {formatBytes(att.sizeBytes)}
            </span>
            {isIndexed && (
              <span
                aria-label="Indicizzato in Knowledge Base"
                title="Indicizzato in Knowledge Base"
                style={{ color: tc.success, fontWeight: 700, fontSize: 10 }}
              >
                KB
              </span>
            )}
          </button>
        );
      })}
    </div>
  );
}

export interface MessageListProps {
  messages: ChatMessage[];
  busyByMessage: Record<string, string | undefined>;
  tc: ThemeColors;
  t: (key: string) => string;
  onCopy: (messageId: string, content: string) => Promise<boolean> | boolean;
  onResend: (messageId: string) => void;
  onDelete: (messageId: string) => void;
  onFeedback: (messageId: string, content: string) => void;
  /** Feedback positivo: conferma esplicita che la risposta e' corretta (Q-learning reward=1.0). */
  onFeedbackPositive?: (messageId: string) => void;
  /** Set di messageId per cui l'utente ha gia' inviato feedback positivo (UI mostra stato confermato). */
  positiveFeedback?: Set<string>;
  lastUserRef: RefObject<HTMLDivElement | null>;
  /** ID progetto corrente: abilita esecuzione comandi dai blocchi codice shell. */
  projectId?: string;
  /** Scelte di proseguimento (next_actions) da mostrare come pulsanti DENTRO la
   *  bolla del messaggio assistant a cui appartengono (a fine proposta, non in un
   *  blocco separato). `runId` identifica il run; i pulsanti vengono attaccati
   *  all'ultimo messaggio assistant di quel run. */
  nextActions?: { runId?: string; choices: NextActionChoice[] };
  /** Timeline meta_step (plan/routing/clarify/fallback/reflection/tool_executed)
   *  per runId. Resa come card collassabili sotto OGNI messaggio assistant con
   *  runId (FIX D6), non piu' in un unico blocco "Decisioni del turno" per il
   *  solo ultimo run. I next_actions sono esclusi (gestiti come pulsanti). */
  metaStepsMap?: Map<string, MetaStepEntry[]>;
  /** Step agente per runId (agentStepsMap di useChat). Sorgente del nastro
   *  attivita' (ADR 0037) insieme a metaStepsMap e traces. */
  agentStepsMap?: Map<string, AgentStep[]>;
  /** Trace gateway della SESSIONE (filtrate per runId dentro il nastro). */
  traces?: AITraceEvent[];
  /** Flag chat.activity_stream_enabled (ADR 0037). OFF (default) = rendering
   *  odierno bit-identico; ON = nastro attivita'. */
  activityStreamEnabled?: boolean;
  /** Soglia densita' del collasso tool (2 compatto / 3 medio / 4 esteso).
   *  Deriva dalla larghezza @container; default 3 (medio). */
  foldThreshold?: FoldThreshold;
}

type CopyFeedbackState = { messageId: string; status: "success" | "error" } | null;

function messageActionIconStyle(
  tc: ThemeColors,
  disabled = false,
  highlight: "success" | "error" | null = null,
) {
  const highlighted = Boolean(highlight);
  const color =
    highlight === "success"
      ? tc.success
      : highlight === "error"
        ? tc.error
        : disabled
          ? tc.textMuted
          : tc.textSecondary;
  return {
    width: 20,
    height: 20,
    padding: 0,
    borderRadius: 4,
    border: "none",
    background: "transparent",
    color,
    fontSize: 12,
    cursor: disabled ? "not-allowed" : "pointer",
    fontFamily: "inherit",
    display: "inline-flex",
    alignItems: "center",
    justifyContent: "center",
    lineHeight: 1,
    opacity: disabled ? 0.6 : highlighted ? 1 : 0.9,
    transform: highlighted ? "scale(1.18)" : "scale(1)",
    boxShadow: highlighted ? `0 0 0 6px ${color}22` : "none",
    transition: "transform 160ms ease, box-shadow 220ms ease, color 180ms ease, opacity 180ms ease",
  } as const;
}

export function MessageList({
  messages,
  busyByMessage,
  tc,
  t,
  onCopy,
  onResend,
  onDelete,
  onFeedback,
  onFeedbackPositive,
  positiveFeedback,
  lastUserRef,
  projectId,
  nextActions,
  metaStepsMap,
  agentStepsMap,
  traces,
  activityStreamEnabled = false,
  foldThreshold = 3,
}: MessageListProps) {
  const [copyFeedback, setCopyFeedback] = useState<CopyFeedbackState>(null);
  const copyFeedbackTimeoutRef = useRef<number | null>(null);

  useEffect(() => {
    return () => {
      if (copyFeedbackTimeoutRef.current !== null) {
        window.clearTimeout(copyFeedbackTimeoutRef.current);
      }
    };
  }, []);

  const handleCopy = async (messageId: string, content: string) => {
    const result = await onCopy(messageId, content);
    const ok = result !== false;
    setCopyFeedback({ messageId, status: ok ? "success" : "error" });
    if (copyFeedbackTimeoutRef.current !== null) {
      window.clearTimeout(copyFeedbackTimeoutRef.current);
    }
    copyFeedbackTimeoutRef.current = window.setTimeout(() => {
      setCopyFeedback((current) => (current?.messageId === messageId ? null : current));
    }, 1100);
  };

  // Filtra messaggi sintetici (auto-continuazione "Continua") prima del raggruppamento.
  // Restano persistiti nel DB per coerenza del run, ma non vanno mostrati come se
  // fossero stati digitati dall'utente. ECCEZIONE: i messaggi sintetici di
  // RISVEGLIO AUTOMATICO di sistema (process_resume) vanno invece resi visibili
  // come banner distintivo, cosi' l'utente capisce che l'agente si e' svegliato
  // da solo (vedi classifySystemWakeup / SystemWakeupBanner).
  const visibleMessages = messages.filter(
    (m) => !m.synthetic || classifySystemWakeup(m) !== null,
  );
  const grouped = groupMessages(visibleMessages);

  // ADR 0037: id dell'ULTIMO messaggio assistant con runId nella lista visibile.
  // Col flag ON e' l'unico turno reso con nastro ESPANSO; i turni assistant
  // precedenti con runId si rendono come riga storica COMPATTA (collassata,
  // espandibile). "Ultimo" e' l'ultimo in ordine di lista, coerente con la
  // logica di isLastUser (che guarda i messaggi successivi).
  const lastAssistantRunMessageId = (() => {
    for (let i = visibleMessages.length - 1; i >= 0; i--) {
      const m = visibleMessages[i];
      if (m.role === "assistant" && m.runId) return m.id;
    }
    return null;
  })();

  return (
    <>
      {grouped.map((item) => {
        if (item.type === "run-group") {
          return (
            <RunSummaryGroup
              key={`run-group-${item.startIdx}`}
              messages={item.messages}
              tc={tc}
            />
          );
        }

        const { message, idx } = item;

        // Risveglio automatico di sistema: rendering dedicato (banner), non la
        // bolla utente standard. Distingue nettamente il turno auto-avviato.
        const wakeup = classifySystemWakeup(message);
        if (wakeup) {
          return (
            <SystemWakeupBanner key={message.id} wakeup={wakeup} tc={tc} t={t} />
          );
        }

        const isUser = message.role === "user";
        const isDeleted = Boolean(message.deletedAt);
        const busyAction = busyByMessage[message.id];
        const isLastUser =
          isUser && messages.slice(idx + 1).every((m) => m.role !== "user");

        return (
          <div
            key={message.id}
            ref={isLastUser ? lastUserRef : undefined}
            style={{
              padding: "8px 10px",
              borderRadius: 10,
              border: `1px solid ${isUser ? tc.accent + "44" : tc.border}`,
              background: isUser ? tc.accentBg : tc.bgCard,
              opacity: isDeleted ? 0.6 : 1,
              alignSelf: isUser ? "flex-end" : "flex-start",
              maxWidth: "96%",
              minWidth: "auto",
              wordBreak: "break-word",
              overflowWrap: "anywhere",
            }}
          >
            {/* Header row: role label + action buttons */}
            <div
              style={{
                display: "flex",
                justifyContent: "space-between",
                alignItems: "center",
                gap: 8,
                marginBottom: 4,
              }}
            >
              <span
                style={{
                  color: isUser ? tc.accent : tc.success,
                  fontWeight: 700,
                  fontSize: 11,
                  whiteSpace: "nowrap",
                }}
              >
                {isUser ? t("chat.you") : t("chat.ai")}
                {isUser && message.resendOfMessageId && (
                  <span
                    style={{
                      color: tc.accent,
                      fontWeight: 700,
                      marginLeft: 6,
                      fontSize: 10,
                      border: `1px solid ${tc.accent}66`,
                      borderRadius: 6,
                      padding: "1px 6px",
                    }}
                    title="Messaggio reinviato"
                    aria-label="Messaggio reinviato"
                  >
                    Reinvio
                  </span>
                )}
                {(message.provider || message.model) && (
                  <span style={{ color: tc.textMuted, fontWeight: 400, marginLeft: 6 }}>
                    [{message.provider ?? "-"}/{message.model ?? "-"}]
                  </span>
                )}
              </span>
              <div style={{ display: "flex", gap: 4, flexWrap: "wrap" }}>
                {(() => {
                  const copyHighlight =
                    copyFeedback?.messageId === message.id ? copyFeedback.status : null;
                  const copyIcon = copyHighlight === "success" ? "✓" : copyHighlight === "error" ? "!" : "⧉";
                  const copyTitle =
                    copyHighlight === "success"
                      ? "Copiato"
                      : copyHighlight === "error"
                        ? "Copia non riuscita"
                        : "Copia";
                  return (
                    <button
                      type="button"
                      onClick={() => void handleCopy(message.id, message.content)}
                      style={messageActionIconStyle(tc, false, copyHighlight)}
                      title={copyTitle}
                      aria-label={copyTitle}
                    >
                      {copyIcon}
                    </button>
                  );
                })()}
                {isUser ? (
                  <button
                    type="button"
                    disabled={Boolean(busyAction)}
                    onClick={() => onResend(message.id)}
                    style={messageActionIconStyle(tc, Boolean(busyAction))}
                    title="Reinvia richiesta"
                    aria-label="Reinvia richiesta"
                  >
                    {busyAction === "resend" ? "…" : "↻"}
                  </button>
                ) : (
                  <>
                    {/* Feedback abilitato SOLO per messaggi con UUID reale dal DB.
                        I messaggi sintetici creati lato frontend hanno id "agent-{runId}"
                        (vedi use-chat.ts::createTerminalMessage) e l'API
                        /feedback-positive | /feedback-error fa Uuid::parse_str
                        che fallisce con "Message id non valido". */}
                    {(() => {
                      const isPersistedUuid = /^[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}$/i.test(message.id);
                      if (!isPersistedUuid) return null;
                      return (
                        <>
                          {onFeedbackPositive && (() => {
                            const alreadyVoted = positiveFeedback?.has(message.id) ?? false;
                            return (
                              <button
                                type="button"
                                disabled={Boolean(busyAction) || alreadyVoted}
                                onClick={() => onFeedbackPositive(message.id)}
                                style={messageActionIconStyle(
                                  tc,
                                  Boolean(busyAction),
                                  alreadyVoted ? "success" : null,
                                )}
                                title={
                                  alreadyVoted
                                    ? "Feedback positivo gia' inviato"
                                    : "Risposta corretta (rinforza apprendimento)"
                                }
                                aria-label="Feedback positivo"
                              >
                                {busyAction === "feedback-positive"
                                  ? "…"
                                  : alreadyVoted
                                    ? "👍"
                                    : "👍🏻"}
                              </button>
                            );
                          })()}
                          <button
                            type="button"
                            disabled={Boolean(busyAction)}
                            onClick={() => onFeedback(message.id, message.content ?? "")}
                            style={messageActionIconStyle(tc, Boolean(busyAction))}
                            title="Segnala errore"
                            aria-label="Segnala errore"
                          >
                            {busyAction === "feedback" ? "…" : "⚠"}
                          </button>
                        </>
                      );
                    })()}
                  </>
                )}
                <button
                  type="button"
                  disabled={Boolean(busyAction)}
                  onClick={() => onDelete(message.id)}
                  style={messageActionIconStyle(tc, Boolean(busyAction))}
                  title="Cancella"
                  aria-label="Cancella"
                >
                  {busyAction === "delete" ? "…" : "\uD83D\uDDD1"}
                </button>
              </div>
            </div>

            {/* Message content */}
            <div style={{ color: tc.text, minWidth: 0, wordBreak: "break-word", overflowWrap: "break-word" }}>
              {isUser ? (
                <MarkdownBlock content={message.content} />
              ) : (message.content ?? "").includes("[Riassunto della conversazione precedente") ? (
                // Riassunti di compattazione: collassati di default (blocco <details>
                // nativo, nessuno stato React) cosi' non sommergono la chat con 4-5
                // riassunti lunghi. L'esito reale resta visibile; il dettaglio e' a un clic.
                <details style={{ opacity: 0.7, fontSize: 13 }}>
                  <summary style={{ cursor: "pointer", fontStyle: "italic", userSelect: "none" }}>
                    Riassunto della compattazione precedente — clic per espandere
                  </summary>
                  <div style={{ marginTop: 8 }}>
                    <MarkdownBlock content={message.content} projectId={projectId} />
                  </div>
                </details>
              ) : (() => {
                // Ragionamento (D4): il backend persiste il thinking in
                // metadata.reasoning (esposto come message.reasoning), NON come
                // prefisso <nexus:thinking> nel content. Preferiamo quello quando
                // presente; parseThinking resta SOLO come fallback per i vecchi
                // messaggi che avessero il tag inline. Niente doppio rendering.
                const parsed = parseThinking(message.content ?? "");
                const reasoning = message.reasoning?.trim() || parsed.thinking;
                const text = parsed.text;
                const { toolUses, cleanText } = extractToolUseBlocks(text);
                return (
                  <>
                    {reasoning && <ThinkingPanel thinking={reasoning} />}
                    {cleanText.trim() && <MarkdownBlock content={cleanText} projectId={projectId} />}
                    {toolUses.length > 0 && <ToolUseBadges toolUses={toolUses} tc={tc} />}
                    {!cleanText.trim() && toolUses.length === 0 && (
                      <span style={{ opacity: 0.6, fontStyle: "italic", fontSize: 12 }}>
                        (nessun contenuto)
                      </span>
                    )}
                  </>
                );
              })()}
            </div>

            {/* Badge di stato del turno (persistente, dal run canonico): rende
                esplicito com'e' finito il run e demarca la fine del turno.
                Per i turni STORICI compatti (flag ON, non l'ultimo run) lo stato
                e' gia' nel badge della riga storica: lo nascondo per non
                duplicarlo. Con flag OFF o sull'ultimo turno resta invariato. */}
            {!isUser &&
              message.runStatus &&
              !(
                activityStreamEnabled &&
                message.runId &&
                message.id !== lastAssistantRunMessageId
              ) && (
                <div style={{ marginTop: 6 }}>
                  <RunStatusBadge status={message.runStatus} tc={tc} />
                </div>
              )}

            {/* ADR 0037: quando il flag activity_stream_enabled e' ON, il
                messaggio assistant con runId mostra il NASTRO ATTIVITA' al posto
                di "Decisioni del turno" + "Mostra step agente". Sorgente:
                metaSteps + steps + traces del run.
                - L'ULTIMO turno assistant con runId -> nastro ESPANSO pieno
                  (MessageActivityStream, segmenti + footer costo).
                - I turni PRECEDENTI -> riga storica COMPATTA (ActivityHistoryRow:
                  badge stato + trail provider colorato + token/costo),
                  collassata ed espandibile al nastro completo.
                Con flag OFF il rendering resta IDENTICO a oggi. */}
            {(() => {
              if (isUser || !message.runId || !activityStreamEnabled) return null;
              const runId = message.runId;
              const runMeta = metaStepsMap?.get(runId) ?? [];
              const runSteps = agentStepsMap?.get(runId) ?? [];
              const runTraces = traces ? tracesForRun(traces, runId) : [];
              const hasRunData = runMeta.length > 0 || runSteps.length > 0 || runTraces.length > 0;
              if (!hasRunData) return null;
              const isLastAssistantRun = message.id === lastAssistantRunMessageId;
              if (isLastAssistantRun) {
                return (
                  <MessageActivityStream
                    runId={runId}
                    metaSteps={runMeta}
                    steps={runSteps}
                    traces={runTraces}
                    foldThreshold={foldThreshold}
                    tc={tc}
                  />
                );
              }
              return (
                <div style={{ marginTop: 6 }}>
                  <ActivityHistoryRow
                    runId={runId}
                    metaSteps={runMeta}
                    steps={runSteps}
                    traces={runTraces}
                    foldThreshold={foldThreshold}
                    runStatus={message.runStatus}
                    totalTokens={message.totalTokens}
                    totalCostUsd={message.totalCost}
                    defaultExpanded={false}
                    tc={tc}
                  />
                </div>
              );
            })()}

            {/* Decisioni del turno (meta_step) per QUESTO messaggio (FIX D6):
                card per-messaggio invece di un unico blocco per l'ultimo run.
                Sorgente: metaStepsMap del run (live SSE o rilette dal DB al
                bootstrap), cosi' restano dopo un reload e sui turni passati.
                Reso solo con flag OFF (con flag ON lo sostituisce il nastro). */}
            {!isUser && !activityStreamEnabled && message.runId && metaStepsMap?.get(message.runId) && (
              <MessageMetaSteps
                steps={metaStepsMap.get(message.runId)!}
                tc={tc}
              />
            )}

            {/* Scelte di proseguimento: pulsanti attaccati a fine proposta DENTRO
                la bolla del messaggio assistant che le ha generate (vicino al
                testo). Sorgente: scelte live (SSE) se disponibili per questo run,
                altrimenti rilette dal DB -> i pulsanti restano dopo un reload e
                sui turni passati. */}
            {!isUser && message.runId && (
              <MessageNextActions
                runId={message.runId}
                liveChoices={
                  nextActions?.runId === message.runId ? nextActions.choices : undefined
                }
                tc={tc}
              />
            )}

            {/* Chip allegati salvati: cliccabili (immagini -> tab raw, testo/binario -> editor). */}
            {isUser && message.attachments && message.attachments.length > 0 && (
              <AttachmentChips attachments={message.attachments} tc={tc} />
            )}

            {/* Pannello step agente (caricamento lazy dal DB): mostrato per
                OGNI run, non solo in modalita' agent, cosi' si vede sempre cosa
                e' stato fatto. Si auto-nasconde se il run non ha prodotto step.
                Con flag ON gli step sono gia' nel nastro attivita': lo nascondo
                per non duplicare. */}
            {!isUser && !activityStreamEnabled && message.runId && (
              <AgentRunStepsInline runId={message.runId} tc={tc} />
            )}

            {/* Usage badge per messaggi assistant con dati token */}
            {!isUser && (message.totalTokens ?? 0) > 0 && (
              <div style={{
                display: "flex",
                alignItems: "center",
                gap: 8,
                marginTop: 6,
                padding: "4px 8px",
                borderRadius: 6,
                background: `${tc.border}33`,
                fontSize: 10,
                color: tc.textMuted,
                flexWrap: "wrap",
              }}>
                {message.provider && message.model && (
                  <span style={{ fontWeight: 600 }}>
                    {message.provider}/{message.model}
                  </span>
                )}
                {(() => {
                  const total = message.totalTokens ?? 0;
                  const lastIn = message.promptTokens ?? 0;
                  const lastOut = message.completionTokens ?? 0;
                  // total e' cumulativo sull'intero run (tutte le iterazioni),
                  // mentre in/out sono dell'ULTIMA chiamata: senza etichetta
                  // "212K (47K in / 332 out)" sembra incongruente (47K+332 != 212K).
                  const cumulative = total > lastIn + lastOut + 50;
                  return (
                    <>
                      <span>{total.toLocaleString("it-IT")} token{cumulative ? " totali" : ""}</span>
                      {lastIn > 0 && (
                        <span>({cumulative ? "ultima chiamata: " : ""}{lastIn.toLocaleString("it-IT")} in / {lastOut.toLocaleString("it-IT")} out)</span>
                      )}
                    </>
                  );
                })()}
                {(message.totalCost ?? 0) > 0 && (
                  <span style={{ color: tc.warning }}>
                    ${message.totalCost!.toFixed(4)} {message.currency ?? "USD"}
                  </span>
                )}
              </div>
            )}
          </div>
        );
      })}
    </>
  );
}
