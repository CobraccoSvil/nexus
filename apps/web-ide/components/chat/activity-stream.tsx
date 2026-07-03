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
import type {
  ActivityStream,
  ActivitySegment,
  ActivityEvent,
  ToolEvent,
  SwitchEvent,
  FoldThreshold,
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
};

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
 *  sempre resa, con motivo e cooldown quando noti. */
function SwitchBand({ sw, tc }: { sw: SwitchEvent; tc: ThemeColors }) {
  const fromColor = providerBaseColor(sw.fromProvider);
  const toColor = providerBaseColor(sw.toProvider);
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
      {(sw.reason || sw.cooldown) && (
        <div style={{ marginTop: 4, fontSize: 11.5, color: tc.textSecondary }}>
          {sw.reason && (
            <>
              Motivo:{" "}
              <code style={codeStyle}>{sw.reason}</code>
            </>
          )}
          {sw.cooldown && (
            <>
              {sw.reason ? " · " : ""}
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
        padding: "7px 10px 7px 42px",
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
      <EventBody event={event} segColor={segColor} tc={tc} />
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
      return (
        <div style={{ minWidth: 0 }}>
          <div style={rowStyle}>
            <span className="nx-as-kind-label" style={kindLabelStyle(segColor)}>
              {EVENT_KIND_LABEL.tool}
            </span>
            {typeof event.iteration === "number" && (
              <span style={metaStyle(tc)}>iter. {event.iteration + 1}</span>
            )}
          </div>
          <div
            style={{
              marginTop: 4,
              display: "flex",
              alignItems: "center",
              gap: 7,
              flexWrap: "wrap",
              fontSize: 12,
              minWidth: 0,
            }}
          >
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
        </div>
      );
    case "folded_tools":
      return (
        <div style={rowStyle}>
          <span style={{ fontSize: 11.5, color: tc.textMuted }}>
            {event.firstIteration != null && event.lastIteration != null
              ? `iter. ${event.firstIteration + 1}–${event.lastIteration + 1} · `
              : ""}
            {event.count} passi {"·"} tutti ok
          </span>
        </div>
      );
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
    default:
      return null;
  }
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
 */
export function ActivityStreamView({
  stream,
  tc,
}: {
  stream: ActivityStream;
  tc: ThemeColors;
  foldThreshold?: FoldThreshold;
}) {
  if (stream.empty) return null;
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
      {stream.segments.map((seg, i) => (
        <SegmentView key={`seg-${i}`} segment={seg} tc={tc} />
      ))}
    </div>
  );
}
