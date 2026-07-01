"use client";

import { useState } from "react";
import ReactMarkdown from "react-markdown";
import { useThemeColors } from "../../lib/theme";
import type { AITraceEvent } from "../../lib/api-client";
import { calcModelCost as calcCost, formatCostUsd as formatCost } from "../../lib/model-catalog";

function humanizeTraceText(raw: string | undefined | null): string {
  if (!raw) return "";
  const text = raw.trim();
  if (!text) return "";
  const errMatch = text.match(/^\[Error:\s*([\s\S]*?)\]?\s*$/);
  const isError = !!errMatch;
  const inner = errMatch ? errMatch[1].trim() : text;
  const low = inner.toLowerCase();
  if (/request too large|too large for|tokens? per min|tpm.*limit|input.*tokens?.*reduced/i.test(inner))
    return "Richiesta troppo grande per il modello (limite token al minuto superato).";
  if (/resourceexhausted|message larger than|larger than max/i.test(inner))
    return "Il provider AI ha rifiutato un payload troppo grande.";
  if (/unauthenticated|invalid api key|401/.test(low)) return "Credenziali del provider AI non valide.";
  if (/deadlineexceeded|timed? ?out|timeout/.test(low)) return "Il provider AI non ha risposto in tempo.";
  if (/rate ?limit|429|quota/.test(low)) return "Limite di richieste del provider AI raggiunto.";
  if (/unavailable|connection refused|503/.test(low)) return "Provider AI momentaneamente non disponibile.";
  const looksTechnical =
    /MetadataMap|status:\s*\w+|details:\s*\[|grpc[\s_-]?status|^\s*[[{]/i.test(inner) ||
    /\borg-[a-z0-9]{10,}/i.test(inner);
  if (looksTechnical) return isError ? "Errore del provider AI." : text;
  return isError ? `${inner}` : text;
}

function stopReasonColor(reason: string, tc: ReturnType<typeof useThemeColors>): string {
  const r = reason.toLowerCase();
  if (r === "end_turn" || r === "stop") return tc.success;
  if (r === "tool_use") return tc.accent;
  if (r.includes("error") || r.includes("fail")) return tc.error;
  return tc.textMuted;
}

// Singola trace compressa — testo lungo comprimibile
function CompactTraceCard({
  trace,
  tc,
}: {
  trace: AITraceEvent;
  tc: ReturnType<typeof useThemeColors>;
}) {
  const [textExpanded, setTextExpanded] = useState(false);
  const safeText = humanizeTraceText(trace.responseText);
  // "lungo" = più di una riga logica (contiene \n o supera ~90 char)
  const isLong = safeText.length > 90 || safeText.includes("\n");
  const firstLine = safeText.split("\n")[0].slice(0, 90) + (safeText.split("\n")[0].length > 90 ? "…" : "");
  const cost = calcCost(
    trace.model,
    trace.inputTokens ?? 0,
    trace.outputTokens ?? 0,
    trace.cacheReadTokens ?? 0,
  );
  const toolNames = (trace.toolCalls ?? []).map((tc) => tc.name).join(", ");

  return (
    <div
      style={{
        border: `1px solid ${tc.border}`,
        borderRadius: 6,
        padding: "6px 10px",
        display: "flex",
        flexDirection: "column",
        gap: 4,
        fontSize: 11,
      }}
    >
      {/* Header: iter · provider/model · timestamp · stopReason */}
      <div style={{ display: "flex", alignItems: "center", gap: 8, flexWrap: "wrap" }}>
        <span style={{ fontWeight: 700, color: tc.accent, fontFamily: 'var(--font-mono)' }}>
          #{trace.iteration}
        </span>
        <span style={{ color: tc.textMuted }}>
          {trace.provider}/{trace.model}
        </span>
        {(trace.inputTokens ?? 0) > 0 && (
          <span style={{ color: tc.textMuted, fontFamily: 'var(--font-mono)' }}>
            ↑{trace.inputTokens} ↓{trace.outputTokens}
            {(trace.cacheReadTokens ?? 0) > 0 && (
              <span style={{ color: tc.success }}> ⚡{trace.cacheReadTokens}</span>
            )}
          </span>
        )}
        {cost !== null && (
          <span
            style={{
              color: cost > 0.05 ? tc.error : cost > 0.01 ? "#f97316" : tc.textMuted,
              fontFamily: 'var(--font-mono)',
            }}
          >
            {formatCost(cost)}
          </span>
        )}
        {trace.stopReason && (
          <span
            style={{
              color: stopReasonColor(trace.stopReason, tc),
              border: `1px solid ${stopReasonColor(trace.stopReason, tc)}`,
              borderRadius: 3,
              padding: "0 4px",
              fontWeight: 600,
            }}
          >
            {trace.stopReason}
          </span>
        )}
        <span style={{ color: tc.textMuted, marginLeft: "auto" }}>
          {trace.timestamp ? new Date(trace.timestamp).toLocaleTimeString() : ""}
        </span>
      </div>

      {/* Tool calls */}
      {toolNames && (
        <div style={{ color: tc.textMuted, fontFamily: 'var(--font-mono)' }}>
          {toolNames}
        </div>
      )}

      {/* Response text — compresso se lungo */}
      {safeText.trim().length > 0 && (
        <div
          style={{
            color: tc.text,
            background: tc.bg,
            border: `1px solid ${tc.border}`,
            borderRadius: 4,
            padding: "4px 8px",
            wordBreak: "break-word",
          }}
        >
          {isLong && !textExpanded ? (
            <span>
              {firstLine}{" "}
              <button
                onClick={() => setTextExpanded(true)}
                style={{
                  background: "none",
                  border: "none",
                  color: tc.accent,
                  cursor: "pointer",
                  padding: 0,
                  fontSize: 11,
                }}
              >
                ▼ altro
              </button>
            </span>
          ) : (
            <>
              <div className="trace-md" style={{ lineHeight: 1.6 }}>
                <ReactMarkdown
                  components={{
                    p: ({ children }) => <p style={{ margin: "2px 0" }}>{children}</p>,
                    // eslint-disable-next-line @typescript-eslint/no-explicit-any
                    code: (({ children }: any) => (
                      <code
                        style={{
                          fontFamily: 'var(--font-mono)',
                          fontSize: 10,
                          background: tc.border + "66",
                          borderRadius: 3,
                          padding: "1px 4px",
                          color: tc.accent,
                        }}
                      >
                        {children}
                      </code>
                    // eslint-disable-next-line @typescript-eslint/no-explicit-any
                    )) as any,
                    // eslint-disable-next-line @typescript-eslint/no-explicit-any
                    pre: (({ children }: any) => (
                      <pre
                        style={{
                          margin: "4px 0",
                          background: tc.bg,
                          border: `1px solid ${tc.border}`,
                          borderRadius: 4,
                          padding: "4px 8px",
                          whiteSpace: "pre-wrap",
                          fontFamily: 'var(--font-mono)',
                          fontSize: 10,
                          color: tc.text,
                        }}
                      >
                        {children}
                      </pre>
                    // eslint-disable-next-line @typescript-eslint/no-explicit-any
                    )) as any,
                  }}
                >
                  {safeText}
                </ReactMarkdown>
              </div>
              {isLong && (
                <button
                  onClick={() => setTextExpanded(false)}
                  style={{
                    background: "none",
                    border: "none",
                    color: tc.accent,
                    cursor: "pointer",
                    padding: 0,
                    fontSize: 11,
                    marginTop: 2,
                  }}
                >
                  ▲ meno
                </button>
              )}
            </>
          )}
        </div>
      )}
    </div>
  );
}

// Componente principale — header collassabile + lista trace
export function InlineTracePanel({ traces }: { traces: AITraceEvent[] }) {
  const tc = useThemeColors();
  const [open, setOpen] = useState(false);

  if (traces.length === 0) return null;

  const totalInput  = traces.reduce((s, t) => s + (t.inputTokens ?? 0), 0);
  const totalOutput = traces.reduce((s, t) => s + (t.outputTokens ?? 0), 0);
  const totalCache  = traces.reduce((s, t) => s + (t.cacheReadTokens ?? 0), 0);
  const totalCost   = traces.reduce<number | null>((acc, t) => {
    const c = calcCost(t.model, t.inputTokens ?? 0, t.outputTokens ?? 0, t.cacheReadTokens ?? 0);
    if (c === null) return acc;
    return (acc ?? 0) + c;
  }, null);

  return (
    <div
      style={{
        marginTop: 6,
        borderRadius: 8,
        border: `1px solid ${tc.border}`,
        background: tc.bgCard,
        overflow: "hidden",
      }}
    >
      {/* Header sempre visibile — click per aprire/chiudere */}
      <button
        onClick={() => setOpen((o) => !o)}
        style={{
          display: "flex",
          alignItems: "center",
          gap: 8,
          width: "100%",
          padding: "6px 10px",
          background: "none",
          border: "none",
          cursor: "pointer",
          color: tc.textMuted,
          fontSize: 11,
          textAlign: "left",
        }}
      >
        <span style={{ color: tc.textMuted }}>Trace AI</span>
        <span style={{ fontFamily: 'var(--font-mono)', color: tc.textMuted }}>
          {traces.length} iter · ↑{totalInput} ↓{totalOutput}
          {totalCache > 0 && <span style={{ color: tc.success }}> ⚡{totalCache}</span>}
        </span>
        {totalCost !== null && (
          <span
            style={{
              fontFamily: 'var(--font-mono)',
              color: totalCost > 0.05 ? tc.error : totalCost > 0.01 ? "#f97316" : tc.textMuted,
            }}
          >
            {formatCost(totalCost)}
          </span>
        )}
        <span style={{ marginLeft: "auto" }}>{open ? "▲" : "▼"}</span>
      </button>

      {/* Lista trace — visibile solo se aperta */}
      {open && (
        <div
          style={{
            padding: "4px 8px 8px",
            display: "flex",
            flexDirection: "column",
            gap: 4,
            borderTop: `1px solid ${tc.border}`,
          }}
        >
          {traces.map((t) => (
            <CompactTraceCard key={`${t.runId}-${t.iteration}`} trace={t} tc={tc} />
          ))}
        </div>
      )}
    </div>
  );
}
