"use client";

// Renderer del "nastro attivita'" della chat (ADR 0037). Consuma il modello
// prodotto dal punto unico di composizione (lib/use-chat/activity-stream.ts) e
// lo disegna come una SPINA SOBRIA (linea neutra, un nodo colorato per provider
// su ogni evento) intervallata da BANDE "Cambio provider" a colore pieno.
//
// Riuso (regola L):
//  - ProviderBadge / providerBaseColor: colori provider autoritativi (mai
//    duplicati qui);
//  - toolLabel: etichette umane dei tool;
//  - PlanChecklist: checklist plan live (TodoUpdated via SSE);
//  - MarkdownBlock: ragionamento.
//
// Densita' adattiva: la larghezza e' gestita dalle @container query in
// globals.css (classi nx-as-*). Le tre invarianti (nodo provider, banda switch,
// esito tool) NON hanno classi che le nascondono: restano a ogni densita'.
//
// Stile: inline + useThemeColors, niente Tailwind, niente emoji nei sorgenti.

import { useState } from "react";
import { useThemeColors } from "../../lib/theme";
import { ProviderBadge, providerBaseColor } from "./provider-badge";
import { PlanChecklist } from "./agent-meta-step-card";
import { toolLabel } from "./tool-labels";
import { MarkdownBlock } from "./markdown-renderer";
import { InlineTruncated, formatStepInput, humanizeToolResult } from "./step-detail";
import { ProviderIcon } from "./provider-icon";
import { capStreamToRecent, switchCauseLabel } from "../../lib/use-chat/activity-stream";
import type {
  ActivityStream,
  ActivitySegment,
  ActivityEvent,
  ToolEvent,
  FoldedToolsEvent,
  SwitchEvent,
  FoldThreshold,
  FigureAdvisory,
  FigureAdvisoryReport,
} from "../../lib/use-chat/activity-stream";

type ThemeColors = ReturnType<typeof useThemeColors>;

/** Converte hex (#RRGGBB) + alpha in rgba. */
function withAlpha(hex: string, alpha: number): string {
  const m = /^#?([0-9a-fA-F]{6})$/.exec(hex);
  if (!m) return hex;
  const v = m[1];
  const r = parseInt(v.slice(0, 2), 16);
  const g = parseInt(v.slice(2, 4), 16);
  const b = parseInt(v.slice(4, 6), 16);
  return `rgba(${r},${g},${b},${alpha})`;
}

// Glifi monospaziati per tipo evento (coerenti col mockup v3). Non-emoji.
const EVENT_GLYPH: Record<ActivityEvent["type"], string> = {
  routing: "→", // freccia
  plan: "□", // quadrato vuoto
  thought: "◇", // rombo vuoto
  tool: "◆", // rombo pieno
  switch: "▲", // triangolo su (mai reso qui: la banda ha il suo header)
  verify: "✓", // check
  context_overflow: "!",
  folded_tools: "…", // ellissi
  subagent: "◈", // rombo con centro (attivita' delegata)
  awaiting_subagents: "⧗", // clessidra (attesa fan-in dei sub-agent)
  council_of_competencies: "◎", // indicatore del Consiglio delle Competenze
  multi_provider_panel: "◉", // panel multi-provider
};

const EVENT_KIND_LABEL: Record<ActivityEvent["type"], string> = {
  routing: "Routing",
  plan: "Piano",
  thought: "Ragionamento",
  tool: "Tool",
  switch: "Cambio provider",
  verify: "Verifica",
  context_overflow: "Contesto",
  folded_tools: "Passi",
  subagent: "Subagente",
  awaiting_subagents: "In attesa",
  council_of_competencies: "Consiglio",
  multi_provider_panel: "Multi-provider",
};

/** Accent della narrazione sub-agente (viola: attivita' delegata, distinto dal
 *  verde tool del run corrente). */
const SUBAGENT_ACCENT = "#8b5cf6";
const COUNCIL_ACCENT = "#0ea5e9";
const MULTI_PROVIDER_ACCENT = "#6366f1";

const FINAL_GATE_PHASES: Record<string, string> = {
  start: "avviata",
  passed: "superata",
  failed: "non superata, nuovo tentativo",
  forced_close: "chiusura al limite tentativi",
};

/** Esito tool -> tag colorato (invariante: sempre visibile). */
function ToolOutcomeTag({ tool, tc }: { tool: ToolEvent; tc: ThemeColors }) {
  if (tool.outcome === "running") {
    return (
      <span style={tagStyle(tc.accent)}>in corso</span>
    );
  }
  const isErr = tool.outcome === "err";
  const color = isErr ? tc.error : "#22c55e";
  return (
    <>
      <span style={tagStyle(color)}>{isErr ? "errore" : "ok"}</span>
      {typeof tool.exitCode === "number" && (
        <span style={{ ...tagStyle(tc.textMuted), fontWeight: 600 }}>exit {tool.exitCode}</span>
      )}
    </>
  );
}

function tagStyle(color: string): React.CSSProperties {
  return {
    fontSize: 9.5,
    fontWeight: 700,
    color,
    background: withAlpha(color, 0.14),
    border: `1px solid ${withAlpha(color, 0.4)}`,
    borderRadius: 5,
    padding: "1px 6px",
    fontFamily: "var(--font-mono)",
    whiteSpace: "nowrap",
    flexShrink: 0,
  };
}

/** Ragionamento inline clampabile (clamp via classe @container, "espandi" a
 *  larghezze strette). */
function ThoughtBlock({ text, tc }: { text: string; tc: ThemeColors }) {
  const [expanded, setExpanded] = useState(false);
  const think = "#a78bfa";
  return (
    <div
      style={{
        marginTop: 5,
        borderLeft: `2px solid ${withAlpha(think, 0.55)}`,
        background: withAlpha(think, 0.08),
        borderRadius: "0 8px 8px 0",
        padding: "6px 10px",
        color: tc.text,
        fontSize: 12,
        minWidth: 0,
      }}
    >
      <div
        style={{
          fontSize: 9.5,
          textTransform: "uppercase",
          letterSpacing: "0.08em",
          color: think,
          fontWeight: 700,
          marginBottom: 2,
        }}
      >
        Ragionamento
      </div>
      <div
        className={expanded ? undefined : "nx-as-thought-body"}
        style={{ lineHeight: 1.5, wordBreak: "break-word" }}
      >
        <MarkdownBlock content={text} />
      </div>
      <button
        type="button"
        onClick={() => setExpanded((v) => !v)}
        style={{
          marginTop: 3,
          fontFamily: "var(--font-mono)",
          fontSize: 10,
          color: think,
          background: "none",
          border: "none",
          cursor: "pointer",
          padding: 0,
        }}
      >
        {expanded ? "▴ comprimi" : "▸ espandi"}
      </button>
    </div>
  );
}

/** Banda "Cambio provider" a colore pieno (gradiente da -> a). Invariante:
 *  sempre resa, con motivo e cooldown quando noti. Il motivo usa l'etichetta
 *  umana della causa strutturata quando nota (punto unico switchCauseLabel,
 *  regola L/M): un rifiuto 4xx o un'esclusione di policy non vanno raccontati
 *  come cooldown ne' come codice grezzo `provider_failover`. */
function SwitchBand({ sw, tc }: { sw: SwitchEvent; tc: ThemeColors }) {
  const fromColor = providerBaseColor(sw.fromProvider);
  const toColor = providerBaseColor(sw.toProvider);
  const causeLabel = switchCauseLabel(sw.cause);
  return (
    <div
      style={{
        margin: "6px 10px 6px 22px",
        borderRadius: 10,
        padding: "9px 11px",
        overflow: "hidden",
        background: `linear-gradient(90deg, ${withAlpha(fromColor, 0.16)}, ${withAlpha(toColor, 0.24)})`,
        border: `1px solid ${withAlpha(toColor, 0.45)}`,
        minWidth: 0,
      }}
    >
      <div
        style={{
          fontSize: 9.5,
          textTransform: "uppercase",
          letterSpacing: "0.1em",
          fontWeight: 800,
          color: toColor,
          marginBottom: 4,
        }}
      >
        {"▲"} Cambio provider
        {sw.attempt && (
          <span style={{ marginLeft: 8, opacity: 0.85, fontWeight: 700 }}>escalation {sw.attempt}</span>
        )}
      </div>
      <div style={{ display: "flex", alignItems: "center", gap: 8, flexWrap: "wrap", fontSize: 12.5, minWidth: 0 }}>
        {(sw.fromProvider || sw.fromModel) && (
          <ProviderBadge provider={sw.fromProvider ?? null} model={sw.fromModel ?? null} />
        )}
        <span style={{ fontSize: 15, color: toColor, flexShrink: 0 }}>{"→"}</span>
        <ProviderBadge provider={sw.toProvider} model={sw.toModel ?? null} />
      </div>
      {(causeLabel || sw.reason || sw.cooldown) && (
        <div style={{ marginTop: 4, fontSize: 11.5, color: tc.textSecondary }}>
          {(causeLabel || sw.reason) && (
            <>
              Motivo:{" "}
              {causeLabel ? (
                // Causa strutturata nota: etichetta umana onesta al posto del
                // codice tecnico (che resta nel payload per gli sviluppatori).
                <span>{causeLabel}</span>
              ) : (
                <code style={codeStyle}>{sw.reason}</code>
              )}
            </>
          )}
          {sw.cooldown && (
            <>
              {causeLabel || sw.reason ? " · " : ""}
              {sw.fromProvider ?? "provider"} in <code style={codeStyle}>cooldown</code> ({sw.cooldown})
            </>
          )}
        </div>
      )}
    </div>
  );
}

const codeStyle: React.CSSProperties = {
  fontFamily: "var(--font-mono)",
  background: "rgba(0,0,0,0.35)",
  padding: "1px 4px",
  borderRadius: 4,
};

/** Riga evento generica con nodo colorato per provider (invariante) e spina
 *  neutra a sinistra. */
function EventRow({
  event,
  segColor,
  tc,
}: {
  event: Exclude<ActivityEvent, SwitchEvent>;
  segColor: string;
  tc: ThemeColors;
}) {
  return (
    <div
      style={{
        position: "relative",
        padding: "7px 34px 7px 42px",
        minWidth: 0,
      }}
    >
      {/* Spina neutra */}
      <span
        aria-hidden
        style={{
          position: "absolute",
          left: 20,
          top: 0,
          bottom: 0,
          width: 2,
          background: tc.border,
        }}
      />
      {/* Nodo colorato per provider (INVARIANTE) */}
      <span
        aria-hidden
        style={{
          position: "absolute",
          left: 15,
          top: 12,
          width: 11,
          height: 11,
          borderRadius: "50%",
          background: segColor,
          border: `2px solid ${tc.bg}`,
          zIndex: 2,
        }}
      />
      {/* Glifo del tipo evento, colorato come il segmento */}
      <span
        aria-hidden
        style={{
          position: "absolute",
          left: 24,
          top: 8,
          fontFamily: "var(--font-mono)",
          fontSize: 9.5,
          color: segColor,
        }}
      >
        {EVENT_GLYPH[event.type]}
      </span>
      {/* Icona provider/model che ha ESEGUITO la riga (tooltip = modello). In
          alto a destra, compatta: scorrendo il nastro si vede chi ha fatto cosa.
          Panel multi-provider: una icona per ogni provider del panel. */}
      {event.type === "multi_provider_panel" &&
      event.panelProviders &&
      event.panelProviders.length > 0 ? (
        <span
          style={{
            position: "absolute",
            right: 8,
            top: 9,
            zIndex: 2,
            display: "inline-flex",
            flexDirection: "column",
            alignItems: "center",
            gap: 4,
            flexShrink: 0,
          }}
        >
          {event.panelProviders.map((p) => (
            <ProviderIcon
              key={`${p.provider}:${p.model ?? ""}`}
              provider={p.provider}
              model={p.model}
              size={18}
            />
          ))}
        </span>
      ) : event.type === "council_of_competencies" ? (
        // Il Consiglio e' un'operazione META: ogni figura gira sul PROPRIO provider
        // (model_purpose tier-aware), non ce n'e' uno solo. Un'icona provider singola
        // qui mostrava "?" (provider del segmento ignoto) ed era fuorviante -> nessuna
        // icona. I provider effettivi si vedono nei sub-agenti delle singole figure.
        null
      ) : (
        event.provider && (
          <span style={{ position: "absolute", right: 8, top: 9, zIndex: 2 }}>
            <ProviderIcon provider={event.provider} model={event.model} />
          </span>
        )
      )}
      <EventBody event={event} segColor={segColor} tc={tc} />
    </div>
  );
}

/** Riga tool: nome + target + esito, cliccabile per espandere Parametri
 *  (input) + Risultato/Errore (troncati). Ripristina il dettaglio per-step nel
 *  nastro (vale sia live sia storico). Espandibile solo se ha input o result. */
function ToolEventBody({
  event,
  segColor,
  tc,
  showProviderIcon = false,
}: {
  event: ToolEvent;
  segColor: string;
  tc: ThemeColors;
  /** true quando reso DENTRO un folded (non passa da EventRow, che gia' mostra
   *  l'icona): allora la mostra nell'header per non perderla. Default false. */
  showProviderIcon?: boolean;
}) {
  const [expanded, setExpanded] = useState(false);
  const hasInput = event.input != null && Object.keys(event.input).length > 0;
  const hasResult = typeof event.result === "string" && event.result.length > 0;
  const expandable = hasInput || hasResult;
  // Risultato umanizzato: JSON {content,status} -> testo leggibile con newline
  // reali; l'errore si legge dai campi strutturati (status/error), non dal testo.
  const humanResult = hasResult ? humanizeToolResult(event.result!) : null;
  // Errore = esito strutturato dello step OPPURE errore segnalato nel risultato.
  const isErr = event.outcome === "err" || Boolean(humanResult?.isError);

  return (
    <div style={{ minWidth: 0 }}>
      <div style={rowStyle}>
        <span className="nx-as-kind-label" style={kindLabelStyle(segColor)}>
          {EVENT_KIND_LABEL.tool}
        </span>
        {typeof event.iteration === "number" && (
          <span style={metaStyle(tc)}>iter. {event.iteration + 1}</span>
        )}
        {showProviderIcon && event.provider && (
          <ProviderIcon provider={event.provider} model={event.model} />
        )}
      </div>
      <div
        onClick={() => expandable && setExpanded((v) => !v)}
        style={{
          marginTop: 4,
          display: "flex",
          alignItems: "center",
          gap: 7,
          flexWrap: "wrap",
          fontSize: 12,
          minWidth: 0,
          cursor: expandable ? "pointer" : "default",
        }}
      >
        {expandable && (
          <span aria-hidden style={{ fontFamily: "var(--font-mono)", fontSize: 9, color: tc.textMuted, flexShrink: 0 }}>
            {expanded ? "▾" : "▸"}
          </span>
        )}
        <span style={{ fontFamily: "var(--font-mono)", fontWeight: 600, color: tc.text, flexShrink: 0 }}>
          {toolLabel(event.name)}
        </span>
        {event.target && (
          <span
            className="nx-as-tool-target"
            title={event.target}
            style={{ fontFamily: "var(--font-mono)", color: tc.textMuted, fontSize: 11 }}
          >
            {event.target}
          </span>
        )}
        <ToolOutcomeTag tool={event} tc={tc} />
      </div>
      {expanded && expandable && (
        <div
          style={{
            marginTop: 6,
            marginLeft: 8,
            paddingLeft: 8,
            borderLeft: `2px solid ${withAlpha(isErr ? tc.error : "#22c55e", 0.4)}`,
            display: "flex",
            flexDirection: "column",
            gap: 8,
            minWidth: 0,
          }}
        >
          {hasInput && (
            <div>
              <div style={detailLabelStyle(tc)}>Parametri</div>
              <InlineTruncated text={formatStepInput(event.input!)} maxLen={400} tc={tc} mono />
            </div>
          )}
          {hasResult && humanResult && (
            <div>
              <div style={{ ...detailLabelStyle(tc), color: isErr ? tc.error : undefined }}>
                {isErr ? "Errore" : "Risultato"}
              </div>
              <InlineTruncated text={humanResult.text} maxLen={500} tc={tc} mono />
            </div>
          )}
        </div>
      )}
    </div>
  );
}

function detailLabelStyle(tc: ThemeColors): React.CSSProperties {
  return {
    fontWeight: 600,
    fontSize: 10,
    textTransform: "uppercase",
    letterSpacing: "0.05em",
    opacity: 0.6,
    marginBottom: 3,
    color: tc.textMuted,
  };
}

/** Riga di tool collassati: header cliccabile "iter. X-Y · N passi · tutti ok";
 *  espansa mostra i singoli ToolEvent (ognuno a sua volta espandibile per
 *  Parametri/Risultato via ToolEventBody). Niente troncamento silenzioso:
 *  l'utente arriva sempre al dettaglio dei singoli step. */
function FoldedToolsBody({
  event,
  segColor,
  tc,
}: {
  event: FoldedToolsEvent;
  segColor: string;
  tc: ThemeColors;
}) {
  const [expanded, setExpanded] = useState(false);
  const range =
    event.firstIteration != null && event.lastIteration != null
      ? `iter. ${event.firstIteration + 1}–${event.lastIteration + 1} · `
      : "";
  return (
    <div style={{ minWidth: 0 }}>
      <div
        onClick={() => setExpanded((v) => !v)}
        style={{
          display: "flex",
          alignItems: "center",
          gap: 6,
          fontSize: 11.5,
          color: tc.textMuted,
          cursor: "pointer",
          minWidth: 0,
        }}
      >
        <span aria-hidden style={{ fontFamily: "var(--font-mono)", fontSize: 9, flexShrink: 0 }}>
          {expanded ? "▾" : "▸"}
        </span>
        <span>
          {range}
          {event.count} passi {"·"} tutti ok
        </span>
      </div>
      {expanded && (
        <div
          style={{
            marginTop: 6,
            marginLeft: 8,
            paddingLeft: 8,
            borderLeft: `2px solid ${withAlpha(segColor, 0.35)}`,
            display: "flex",
            flexDirection: "column",
            gap: 6,
            minWidth: 0,
          }}
        >
          {event.tools.map((tool, i) => (
            <ToolEventBody
              key={`folded-tool-${i}`}
              event={tool}
              segColor={segColor}
              tc={tc}
              showProviderIcon
            />
          ))}
        </div>
      )}
    </div>
  );
}

function EventBody({
  event,
  segColor,
  tc,
}: {
  event: Exclude<ActivityEvent, SwitchEvent>;
  segColor: string;
  tc: ThemeColors;
}) {
  switch (event.type) {
    case "routing":
      return (
        <div style={rowStyle}>
          <span className="nx-as-kind-label" style={kindLabelStyle(segColor)}>
            {EVENT_KIND_LABEL.routing}
          </span>
          <span style={{ fontSize: 12.5, color: tc.text }}>
            {event.intent ? `intent: ${event.intent}` : "ha scelto"}
          </span>
          {event.profile && <span style={metaStyle(tc)}>{event.profile}</span>}
          {event.behaviorMode && <span style={metaStyle(tc)}>{event.behaviorMode}</span>}
          {typeof event.tokenBudget === "number" && (
            <span style={metaStyle(tc)}>budget {event.tokenBudget.toLocaleString("it-IT")}</span>
          )}
        </div>
      );
    case "plan":
      return (
        <div style={{ minWidth: 0 }}>
          <div style={rowStyle}>
            <span className="nx-as-kind-label" style={kindLabelStyle(segColor)}>
              {EVENT_KIND_LABEL.plan}
            </span>
          </div>
          <div style={{ marginTop: 4 }}>
            <PlanChecklist todos={event.todos} />
          </div>
        </div>
      );
    case "thought":
      return <ThoughtBlock text={event.text} tc={tc} />;
    case "tool":
      return <ToolEventBody event={event} segColor={segColor} tc={tc} />;
    case "folded_tools":
      return <FoldedToolsBody event={event} segColor={segColor} tc={tc} />;
    case "verify":
      return (
        <div style={rowStyle}>
          <span className="nx-as-kind-label" style={kindLabelStyle("#22c55e")}>
            {EVENT_KIND_LABEL.verify}
          </span>
          <span style={{ fontSize: 12.5, color: tc.text }}>
            final_gate {event.phase ? (FINAL_GATE_PHASES[event.phase] ?? event.phase) : ""}
          </span>
          {typeof event.cycle === "number" && (
            <span style={metaStyle(tc)}>
              tentativo {event.maxCycles ? `${event.cycle}/${event.maxCycles}` : event.cycle}
            </span>
          )}
        </div>
      );
    case "context_overflow":
      return (
        <div style={rowStyle}>
          <span className="nx-as-kind-label" style={kindLabelStyle(tc.error)}>
            {EVENT_KIND_LABEL.context_overflow}
          </span>
          <span style={{ fontSize: 12.5, color: tc.error }}>
            {event.detail ?? "contesto oltre il limite"}
          </span>
        </div>
      );
    case "subagent": {
      // Errore = fase failed o esito strutturato del tool inoltrato (regola M).
      const isErr = event.phase === "failed" || event.isError === true;
      const accent = isErr ? tc.error : SUBAGENT_ACCENT;
      // Id corto del sub-run (primi 4 hex del subagentRunId): distingue i sub-run
      // di un batch PARALLELO (dispatch_subagents), i cui eventi si interlacciano
      // sul canale del padre e altrimenti sarebbero indistinguibili quando dello
      // stesso kind. Presente solo se il meta-step porta il correlation_id.
      const shortId = event.subagentRunId ? event.subagentRunId.slice(0, 4) : undefined;
      return (
        <div style={{ minWidth: 0 }}>
          <div style={rowStyle}>
            <span className="nx-as-kind-label" style={kindLabelStyle(accent)}>
              {EVENT_KIND_LABEL.subagent}
            </span>
            {shortId && (
              <span
                style={{
                  fontFamily: "var(--font-mono)",
                  fontSize: 10.5,
                  color: accent,
                  opacity: 0.85,
                }}
              >
                #{shortId}
              </span>
            )}
            <span style={{ fontSize: 12.5, color: isErr ? tc.error : tc.text }}>
              {event.title}
            </span>
            {event.phase === "completed" && typeof event.costUsd === "number" && event.costUsd > 0 && (
              <span style={metaStyle(tc)}>${event.costUsd.toFixed(4)}</span>
            )}
          </div>
          {(event.phase === "completed" || event.phase === "failed") && event.summary && (
            <div
              style={{
                marginTop: 3,
                fontSize: 12,
                color: tc.textMuted,
                whiteSpace: "pre-wrap",
                overflowWrap: "anywhere",
              }}
            >
              {event.summary}
            </div>
          )}
        </div>
      );
    }
    case "awaiting_subagents":
      // Attesa fan-in: il run PADRE e' sospeso finche' i sub-agent completano.
      // NON e' un errore ne' una fine: la narrazione dei figli (subagent)
      // continua sopra/sotto. Riga sobria col conteggio, se noto.
      return (
        <div style={rowStyle}>
          <span className="nx-as-kind-label" style={kindLabelStyle(SUBAGENT_ACCENT)}>
            {EVENT_KIND_LABEL.awaiting_subagents}
          </span>
          <span style={{ fontSize: 12.5, color: tc.text }}>
            {typeof event.count === "number" && event.count > 0
              ? `In attesa di ${event.count} sub-agent in background...`
              : "In attesa dei sub-agent in background..."}
          </span>
        </div>
      );
    case "council_of_competencies":
      return (
        <div
          style={{
            minWidth: 0,
            borderRadius: 10,
            border: `1px solid ${withAlpha(event.degraded ? "#f59e0b" : event.phase === "convening" ? COUNCIL_ACCENT : COUNCIL_ACCENT, 0.35)}`,
            background: withAlpha(event.degraded ? "#f59e0b" : COUNCIL_ACCENT, 0.08),
            padding: "7px 9px",
          }}
        >
          <div style={rowStyle}>
            <span
              className="nx-as-kind-label"
              style={kindLabelStyle(event.degraded ? "#f59e0b" : COUNCIL_ACCENT)}
            >
              {EVENT_KIND_LABEL.council_of_competencies}
            </span>
            <span style={{ fontSize: 12.5, fontWeight: 700, color: tc.text }}>
              {event.productName}
            </span>
            <span style={tagStyle(event.degraded ? "#f59e0b" : COUNCIL_ACCENT)}>
              {event.phase === "convening"
                ? "in corso"
                : event.degraded
                  ? "degradato"
                  : "attivo"}
            </span>
          </div>
          <div style={{ marginTop: 3, fontSize: 11.5, color: tc.textMuted }}>
            {event.phase === "convening"
              ? typeof event.completedCount === "number" &&
                typeof event.figureCount === "number" &&
                event.figureCount > 0
                ? `Convocazione figure (${event.completedCount}/${event.figureCount})...`
                : "Convocazione delle figure del consiglio in corso..."
              : event.degraded
                ? event.degradationReason ??
                  "Gate attivato ma la convocazione non ha prodotto una sintesi valida."
                : "Attivato dall'analisi agentica/deterministica di complessita' e ambito della richiesta."}
          </div>
          {event.figureTasks && event.figureTasks.length > 0 ? (
            <ul
              style={{
                margin: "6px 0 0",
                paddingLeft: 0,
                listStyle: "none",
                fontSize: 11,
                color: tc.textMuted,
              }}
            >
              {event.figureTasks.map((task) => (
                <li
                  key={task.kind}
                  style={{
                    display: "flex",
                    alignItems: "center",
                    gap: 6,
                    padding: "2px 0",
                  }}
                >
                  <span
                    style={{
                      width: 8,
                      height: 8,
                      borderRadius: "50%",
                      flexShrink: 0,
                      background:
                        task.status === "done"
                          ? "#22c55e"
                          : task.status === "failed"
                            ? tc.error
                            : task.status === "running"
                              ? COUNCIL_ACCENT
                              : withAlpha(tc.textMuted, 0.5),
                      opacity: task.status === "running" ? 0.85 : 1,
                    }}
                  />
                  <span
                    style={{
                      fontWeight: 600,
                      color:
                        task.status === "failed"
                          ? tc.error
                          : task.status === "done"
                            ? tc.text
                            : tc.textMuted,
                    }}
                  >
                    {task.kind.replace(/_/g, " ")}
                  </span>
                </li>
              ))}
            </ul>
          ) : null}
          {event.figureReports && event.figureReports.length > 0 ? (
            <div style={{ marginTop: 6 }}>
              <div
                style={{
                  fontSize: 10,
                  fontWeight: 700,
                  color: tc.textMuted,
                  textTransform: "uppercase",
                  letterSpacing: 0.3,
                }}
              >
                Pareri delle figure
              </div>
              <ul style={{ margin: "3px 0 0", padding: 0, listStyle: "none" }}>
                {event.figureReports.map((r) => (
                  <FigureReportRow key={r.kind} report={r} tc={tc} />
                ))}
              </ul>
            </div>
          ) : null}
        </div>
      );
    case "multi_provider_panel":
      return (
        <div
          style={{
            minWidth: 0,
            borderRadius: 10,
            border: `1px solid ${withAlpha(MULTI_PROVIDER_ACCENT, 0.35)}`,
            background: withAlpha(MULTI_PROVIDER_ACCENT, 0.08),
            padding: "7px 9px",
          }}
        >
          <div style={rowStyle}>
            <span className="nx-as-kind-label" style={kindLabelStyle(MULTI_PROVIDER_ACCENT)}>
              {EVENT_KIND_LABEL.multi_provider_panel}
            </span>
            <span style={{ fontSize: 12.5, fontWeight: 700, color: tc.text }}>
              {event.productName}
            </span>
            <span style={tagStyle(event.degraded ? tc.warning : MULTI_PROVIDER_ACCENT)}>
              {event.degraded ? "degradato" : "attivo"}
            </span>
          </div>
          <div style={{ marginTop: 3, fontSize: 11.5, color: tc.textMuted }}>
            {event.degraded
              ? event.degradationReason ??
                "Provider distinti insufficienti: panel multi-provider non convocato."
              : typeof event.providerCount === "number" && event.providerCount > 0
                ? `${event.providerCount} provider distinti hanno analizzato la richiesta.`
                : "Analisi parallela su provider/modelli distinti tramite routing tier-aware."}
          </div>
        </div>
      );
    default:
      return null;
  }
}

/** Verdetto strutturato della figura -> colore + etichetta umana (identificatori
 *  canonici backend, regola N: proceed|proceed_with_changes|block). */
function verdictMeta(
  v: string | undefined,
  tc: ThemeColors,
): { color: string; label: string } {
  switch (v) {
    case "proceed":
      return { color: "#22c55e", label: "procede" };
    case "proceed_with_changes":
      return { color: "#f59e0b", label: "procede con modifiche" };
    case "block":
      return { color: tc.error, label: "blocca" };
    default:
      return { color: tc.textMuted, label: v ?? "n/d" };
  }
}

/** Severita' rischio (backend: alta|media|bassa) -> colore. */
function severityColor(sev: string, tc: ThemeColors): string {
  const s = sev.toLowerCase();
  if (s === "alta" || s === "high") return tc.error;
  if (s === "media" || s === "medium") return "#f59e0b";
  return tc.textMuted;
}

/** Estrae severity/description da un rischio (payload backend non tipizzato). */
function riskParts(risk: Record<string, unknown>): {
  severity: string;
  description: string;
} {
  const severity = typeof risk.severity === "string" ? risk.severity : "";
  const description =
    typeof risk.description === "string"
      ? risk.description
      : typeof risk.detail === "string"
        ? risk.detail
        : "";
  return { severity, description };
}

/** Sezione a elenco puntato del parere (requisiti / raccomandazioni / ...). */
function AdvisorySection({
  title,
  items,
  tc,
}: {
  title: string;
  items?: string[];
  tc: ThemeColors;
}) {
  if (!items || items.length === 0) return null;
  return (
    <div style={{ marginTop: 4 }}>
      <div style={{ fontWeight: 600, color: tc.text, fontSize: 10.5 }}>{title}</div>
      <ul style={{ margin: "2px 0 0", paddingLeft: 14, listStyle: "disc" }}>
        {items.map((it, i) => (
          <li key={i}>{it}</li>
        ))}
      </ul>
    </div>
  );
}

/** Corpo completo del parere di una figura: requisiti, rischi (per severita'),
 *  raccomandazioni, osservazioni. */
function AdvisoryBody({ advisory, tc }: { advisory: FigureAdvisory; tc: ThemeColors }) {
  return (
    <>
      <AdvisorySection title="Requisiti" items={advisory.requirements} tc={tc} />
      {advisory.risks && advisory.risks.length > 0 ? (
        <div style={{ marginTop: 4 }}>
          <div style={{ fontWeight: 600, color: tc.text, fontSize: 10.5 }}>Rischi</div>
          <ul style={{ margin: "2px 0 0", paddingLeft: 14, listStyle: "disc" }}>
            {advisory.risks.map((risk, i) => {
              const { severity, description } = riskParts(risk);
              return (
                <li key={i}>
                  {severity ? (
                    <span style={{ ...tagStyle(severityColor(severity, tc)), marginRight: 5 }}>
                      {severity}
                    </span>
                  ) : null}
                  {description}
                </li>
              );
            })}
          </ul>
        </div>
      ) : null}
      <AdvisorySection title="Raccomandazioni" items={advisory.recommendations} tc={tc} />
      <AdvisorySection title="Osservazioni" items={advisory.concerns} tc={tc} />
    </>
  );
}

/** Riga espandibile per il parere di UNA figura del consiglio. Il testo completo
 *  (advisory) e' sempre leggibile su click, non solo in caso di degradazione. */
function FigureReportRow({
  report,
  tc,
}: {
  report: FigureAdvisoryReport;
  tc: ThemeColors;
}) {
  const [open, setOpen] = useState(false);
  const advisory = report.advisory;
  const verdict = advisory?.verdict ?? report.advisory_verdict;
  const vm = verdictMeta(verdict, tc);
  const failed = report.status !== "advisory_ok";
  const hasBody =
    !!advisory &&
    ((advisory.requirements?.length ?? 0) > 0 ||
      (advisory.risks?.length ?? 0) > 0 ||
      (advisory.recommendations?.length ?? 0) > 0 ||
      (advisory.concerns?.length ?? 0) > 0);
  const expandable = hasBody || (failed && !!report.detail_message) || !!report.provider;
  return (
    <li style={{ borderTop: `1px solid ${withAlpha(tc.textMuted, 0.15)}` }}>
      <button
        type="button"
        onClick={() => expandable && setOpen((o) => !o)}
        style={{
          display: "flex",
          alignItems: "center",
          gap: 6,
          width: "100%",
          background: "none",
          border: "none",
          padding: "3px 0",
          cursor: expandable ? "pointer" : "default",
          textAlign: "left",
          color: "inherit",
          font: "inherit",
        }}
      >
        <span style={{ fontSize: 9, color: tc.textMuted, width: 10, flexShrink: 0 }}>
          {expandable ? (open ? "▾" : "▸") : ""}
        </span>
        <span style={{ fontSize: 11, fontWeight: 600, color: failed ? tc.error : tc.text }}>
          {report.kind.replace(/_/g, " ")}
        </span>
        {report.provider ? (
          <span
            style={{
              fontSize: 9.5,
              fontFamily: "var(--font-mono)",
              color: providerBaseColor(report.provider),
              opacity: 0.9,
              whiteSpace: "nowrap",
            }}
          >
            {report.provider}
          </span>
        ) : null}
        <span style={{ ...tagStyle(vm.color), marginLeft: "auto" }}>{vm.label}</span>
      </button>
      {open ? (
        <div style={{ padding: "1px 0 5px 16px", fontSize: 11, color: tc.textMuted }}>
          {report.provider ? (
            <div style={{ marginBottom: 4, fontSize: 10.5, fontFamily: "var(--font-mono)" }}>
              <span style={{ color: tc.textMuted }}>Provider: </span>
              <span style={{ color: providerBaseColor(report.provider) }}>
                {report.provider}
                {report.model ? ` / ${report.model}` : ""}
              </span>
            </div>
          ) : null}
          {failed && report.detail_message ? (
            <div style={{ color: tc.error, marginBottom: advisory ? 4 : 0 }}>
              {report.detail_message}
            </div>
          ) : null}
          {advisory ? <AdvisoryBody advisory={advisory} tc={tc} /> : null}
        </div>
      ) : null}
    </li>
  );
}

const rowStyle: React.CSSProperties = {
  display: "flex",
  alignItems: "baseline",
  gap: 7,
  flexWrap: "wrap",
  minWidth: 0,
};

function kindLabelStyle(color: string): React.CSSProperties {
  return {
    fontSize: 9.5,
    fontFamily: "var(--font-mono)",
    textTransform: "uppercase",
    letterSpacing: "0.09em",
    color,
    fontWeight: 700,
    flexShrink: 0,
  };
}

function metaStyle(tc: ThemeColors): React.CSSProperties {
  return { color: tc.textMuted, fontSize: 11.5 };
}

/** Un segmento: eventuale banda switch in testa, poi la spina di eventi. */
function SegmentView({ segment, tc }: { segment: ActivitySegment; tc: ThemeColors }) {
  const segColor = providerBaseColor(segment.provider);
  return (
    <div style={{ minWidth: 0 }}>
      {segment.openedBySwitch && segment.switch && <SwitchBand sw={segment.switch} tc={tc} />}
      <div style={{ position: "relative", padding: "4px 10px 8px 0", minWidth: 0 }}>
        {segment.events.map((ev, i) =>
          ev.type === "switch" ? null : (
            <EventRow key={`ev-${i}`} event={ev} segColor={segColor} tc={tc} />
          ),
        )}
      </div>
    </div>
  );
}

/**
 * Renderer principale del nastro attivita' di UN run.
 * @param stream modello prodotto da composeActivityStream
 * @param tc     tema
 * @param foldThreshold soglia densita' gia' applicata a monte (informativa)
 * @param liveCap se valorizzato (>0), CAPPA il nastro agli ultimi `liveCap`
 *   eventi non-switch con un toggle "Mostra tutto" (usato SOLO nel rendering
 *   LIVE per non esplodere sui run lunghi). Lo storico non lo passa -> mostra
 *   tutto. Le bande "Cambio provider" restano sempre visibili.
 */
export function ActivityStreamView({
  stream,
  tc,
  liveCap,
}: {
  stream: ActivityStream;
  tc: ThemeColors;
  foldThreshold?: FoldThreshold;
  liveCap?: number;
}) {
  const [showAll, setShowAll] = useState(false);
  if (stream.empty) return null;

  // Cap solo se liveCap valorizzato e l'utente non ha chiesto "Mostra tutto".
  const capped =
    liveCap && liveCap > 0 && !showAll ? capStreamToRecent(stream, liveCap) : null;
  const renderStream = capped ? capped.stream : stream;
  const hiddenCount = capped?.hiddenCount ?? 0;
  const totalEvents = capped?.totalEvents ?? 0;

  return (
    <div
      data-testid="activity-stream"
      style={{
        marginTop: 6,
        border: `1px solid ${tc.border}`,
        borderRadius: 10,
        background: tc.bgCard,
        overflow: "hidden",
        minWidth: 0,
      }}
    >
      {/* Toggle cap live: appare solo se ci sono eventi nascosti o se
          l'espansione e' attiva (per poter tornare al recente). */}
      {liveCap && liveCap > 0 && (hiddenCount > 0 || showAll) && (
        <button
          type="button"
          onClick={() => setShowAll((v) => !v)}
          aria-expanded={showAll}
          style={{
            display: "inline-flex",
            alignItems: "center",
            gap: 6,
            background: "transparent",
            border: "none",
            color: tc.textMuted,
            cursor: "pointer",
            fontSize: 11,
            padding: "6px 10px 2px",
          }}
        >
          <span aria-hidden style={{ fontFamily: "var(--font-mono)" }}>
            {showAll ? "▾" : "▸"}
          </span>
          {showAll
            ? "Mostra solo i recenti"
            : `Mostra tutti gli ${totalEvents} eventi (${hiddenCount} nascosti)`}
        </button>
      )}
      {renderStream.segments.map((seg, i) => (
        <SegmentView key={`seg-${i}`} segment={seg} tc={tc} />
      ))}
    </div>
  );
}
