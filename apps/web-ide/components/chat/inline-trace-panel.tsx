"use client";

import { useState } from "react";
import ReactMarkdown from "react-markdown";
import { useThemeColors } from "../../lib/theme";
import type { AITraceEvent } from "../../lib/api-client";

// Prezzi per milione di token (USD) — sincronizzati con ai-trace-panel.tsx
const MODEL_PRICING: Record<string, { input: number; output: number; cacheRead?: number }> = {
  "claude-opus-4-6":             { input: 5.0,   output: 25.0,  cacheRead: 0.50 },
  "claude-sonnet-4-6":           { input: 3.0,   output: 15.0,  cacheRead: 0.30 },
  "claude-haiku-4-5-20251001":   { input: 0.80,  output: 4.0,   cacheRead: 0.08 },
  "claude-3-haiku-20240307":     { input: 0.25,  output: 1.25  },
  "o3":                          { input: 10.0,  output: 40.0  },
  "o4-mini":                     { input: 1.10,  output: 4.40  },
  "gpt-4.1":                     { input: 2.0,   output: 8.0   },
  "gpt-4.1-mini":                { input: 0.40,  output: 1.60  },
  "gpt-4.1-nano":                { input: 0.10,  output: 0.40  },
  "gpt-4o":                      { input: 2.50,  output: 10.0  },
  "gpt-4o-mini":                 { input: 0.15,  output: 0.60  },
  "gpt-4-turbo":                 { input: 10.0,  output: 30.0  },
  "o1":                          { input: 15.0,  output: 60.0  },
  "o1-mini":                     { input: 3.0,   output: 12.0  },
  "gemini-2.5-pro":              { input: 1.25,  output: 10.0  },
  "gemini-2.5-flash":            { input: 0.15,  output: 0.60  },
  "gemini-2.5-flash-lite":       { input: 0.10,  output: 0.40  },
  "gemini-2.0-flash":            { input: 0.10,  output: 0.40  },
  "gemini-1.5-pro":              { input: 1.25,  output: 5.0   },
  "gemini-1.5-flash":            { input: 0.075, output: 0.30  },
  "deepseek-chat":               { input: 0.28,  output: 0.42  },
  "deepseek-reasoner":           { input: 0.55,  output: 2.19  },
  "deepseek-coder":              { input: 0.28,  output: 0.42  },
  "mistral-large-2411":          { input: 2.0,   output: 6.0   },
  "mistral-small-4":             { input: 0.15,  output: 0.60  },
  "codestral-latest":            { input: 0.20,  output: 0.60  },
  "open-mistral-nemo":           { input: 0.15,  output: 0.15  },
};

function calcCost(model: string, input: number, output: number, cache: number): number | null {
  const key = Object.keys(MODEL_PRICING).find((k) => model.toLowerCase().includes(k.toLowerCase()));
  if (!key) return null;
  const p = MODEL_PRICING[key];
  const billableInput = Math.max(0, input - cache);
  return (billableInput * p.input + cache * (p.cacheRead ?? p.input * 0.1) + output * p.output) / 1_000_000;
}

function formatCost(usd: number): string {
  if (usd <= 0)      return "$0.000";
  if (usd < 0.0001)  return "< $0.0001";
  if (usd < 0.001)   return `$${usd.toFixed(5)}`;
  if (usd < 0.01)    return `$${usd.toFixed(4)}`;
  return `$${usd.toFixed(3)}`;
}

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
        <span style={{ fontWeight: 700, color: tc.accent, fontFamily: '"JetBrains Mono", monospace' }}>
          #{trace.iteration}
        </span>
        <span style={{ color: tc.textMuted }}>
          {trace.provider}/{trace.model}
        </span>
        {(trace.inputTokens ?? 0) > 0 && (
          <span style={{ color: tc.textMuted, fontFamily: '"JetBrains Mono", monospace' }}>
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
              fontFamily: '"JetBrains Mono", monospace',
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
        <div style={{ color: tc.textMuted, fontFamily: '"JetBrains Mono", monospace' }}>
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
                          fontFamily: '"JetBrains Mono", monospace',
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
                          fontFamily: '"JetBrains Mono", monospace',
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
        <span style={{ fontFamily: '"JetBrains Mono", monospace', color: tc.textMuted }}>
          {traces.length} iter · ↑{totalInput} ↓{totalOutput}
          {totalCache > 0 && <span style={{ color: tc.success }}> ⚡{totalCache}</span>}
        </span>
        {totalCost !== null && (
          <span
            style={{
              fontFamily: '"JetBrains Mono", monospace',
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
