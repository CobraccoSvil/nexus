"use client";

import React, { useEffect, useRef, useState } from "react";
import ReactMarkdown from "react-markdown";
import { useThemeColors } from "../../lib/theme";
import type { AITraceEvent } from "../../lib/api-client";

interface AITracePanelProps {
  traces: AITraceEvent[];
  onClear?: () => void;
}

// Prezzi per milione di token (USD) — aggiornati aprile 2026
const MODEL_PRICING: Record<string, { input: number; output: number; cacheRead?: number }> = {
  // Anthropic
  "claude-opus-4-6":             { input: 5.0,   output: 25.0,  cacheRead: 0.50 },
  "claude-sonnet-4-6":           { input: 3.0,   output: 15.0,  cacheRead: 0.30 },
  "claude-haiku-4-5-20251001":   { input: 0.80,  output: 4.0,   cacheRead: 0.08 },
  "claude-3-haiku-20240307":     { input: 0.25,  output: 1.25  },
  // OpenAI
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
  // Google
  "gemini-2.5-pro":              { input: 1.25,  output: 10.0  },
  "gemini-2.5-flash":            { input: 0.15,  output: 0.60  },
  "gemini-2.5-flash-lite":       { input: 0.10,  output: 0.40  },
  "gemini-2.0-flash":            { input: 0.10,  output: 0.40  },
  "gemini-1.5-pro":              { input: 1.25,  output: 5.0   },
  "gemini-1.5-flash":            { input: 0.075, output: 0.30  },
  // DeepSeek
  "deepseek-chat":               { input: 0.28,  output: 0.42  },
  "deepseek-reasoner":           { input: 0.55,  output: 2.19  },
  "deepseek-coder":              { input: 0.28,  output: 0.42  },
  // Mistral
  "mistral-large-2411":          { input: 2.0,   output: 6.0   },
  "mistral-small-4":             { input: 0.15,  output: 0.60  },
  "codestral-latest":            { input: 0.20,  output: 0.60  },
  "open-mistral-nemo":           { input: 0.15,  output: 0.15  },
};

function calcCost(
  model: string,
  inputTokens: number,
  outputTokens: number,
  cacheReadTokens: number,
): number | null {
  const key = Object.keys(MODEL_PRICING).find((k) => model.toLowerCase().includes(k.toLowerCase()));
  if (!key) return null;
  const p = MODEL_PRICING[key];
  // I cacheReadTokens NON si sottraggono dagli inputTokens: sono token già contati nell'input
  // che vengono fatturati a tariffa ridotta (cacheRead) invece che a tariffa piena (input).
  // Formula corretta: (input - cache) * price_in + cache * price_cache + output * price_out
  const billableInput = Math.max(0, inputTokens - cacheReadTokens);
  const inputCost  = (billableInput   * p.input)                       / 1_000_000;
  const cacheCost  = (cacheReadTokens * (p.cacheRead ?? p.input * 0.1)) / 1_000_000;
  const outputCost = (outputTokens    * p.output)                       / 1_000_000;
  return inputCost + cacheCost + outputCost;
}

function formatCost(usd: number): string {
  if (usd <= 0)     return `$0.000`;
  if (usd < 0.0001) return `< $0.0001`;
  if (usd < 0.001)  return `$${usd.toFixed(5)}`;
  if (usd < 0.01)   return `$${usd.toFixed(4)}`;
  return `$${usd.toFixed(3)}`;
}

function stopReasonColor(reason: string, tc: ReturnType<typeof useThemeColors>): string {
  const r = reason.toLowerCase();
  if (r === "end_turn" || r === "stop") return tc.success;
  if (r === "tool_use") return tc.accent;
  if (r.includes("error") || r.includes("fail")) return tc.error;
  return tc.textMuted;
}

function formatTimestamp(ts: string): string {
  try {
    return new Date(ts).toLocaleTimeString();
  } catch {
    return ts;
  }
}

// Sanitizza il responseText: i provider scrivono "[Error: <raw>]" con dentro JSON,
// status gRPC, MetadataMap, dettagli rate-limit. Convertiamo in italiano umano.
// Niente JSON/dettagli grezzi nell'UI — vanno solo nei log backend.
function humanizeTraceText(raw: string | undefined | null): string {
  if (!raw) return "";
  let text = raw.trim();
  if (!text) return "";

  // Estrai contenuto da [Error: ...]
  const errMatch = text.match(/^\[Error:\s*([\s\S]*?)\]?\s*$/);
  const isError = !!errMatch;
  const inner = errMatch ? errMatch[1].trim() : text;
  const low = inner.toLowerCase();

  // Pattern noti → messaggio italiano
  if (/request too large|too large for|tokens? per min|tpm.*limit|input.*tokens?.*reduced/i.test(inner)) {
    return "⚠ Richiesta troppo grande per il modello (limite token al minuto superato). Riduci il contesto o usa un modello con TPM più alto.";
  }
  if (/resourceexhausted|message larger than|larger than max/i.test(inner)) {
    return "⚠ Il provider AI ha rifiutato un payload troppo grande. Riduci il contesto e riprova.";
  }
  if (/unauthenticated|invalid api key|401/.test(low)) {
    return "⚠ Credenziali del provider AI non valide.";
  }
  if (/deadlineexceeded|timed? ?out|timeout/.test(low)) {
    return "⚠ Il provider AI non ha risposto in tempo.";
  }
  if (/rate ?limit|429|quota/.test(low)) {
    return "⚠ Limite di richieste del provider AI raggiunto. Riprova tra poco.";
  }
  if (/unavailable|connection refused|503/.test(low)) {
    return "⚠ Provider AI momentaneamente non disponibile.";
  }
  if (/server.*riavviato|riavviato durante/i.test(inner)) {
    return inner; // già umano
  }

  // Contenuto tecnico grezzo (JSON, MetadataMap, status:, stack) → messaggio generico
  const looksTechnical =
    /MetadataMap|status:\s*\w+|details:\s*\[|grpc[\s_-]?status|^\s*[[{]/i.test(inner) ||
    /\borg-[a-z0-9]{10,}/i.test(inner); // org-id OpenAI
  if (looksTechnical) {
    return isError ? "⚠ Errore del provider AI. Controlla i log per i dettagli." : text;
  }

  return isError ? `⚠ ${inner}` : text;
}

function TraceCard({
  trace,
  tc,
}: {
  trace: AITraceEvent;
  tc: ReturnType<typeof useThemeColors>;
}) {
  const [expanded, setExpanded] = useState(false);
  const safeText = humanizeTraceText(trace.responseText);
  const preview = safeText.slice(0, 300);
  const hasMore = safeText.length > 300;

  return (
    <div
      style={{
        border: `1px solid ${tc.border}`,
        borderRadius: 8,
        background: tc.bgCard,
        padding: "10px 12px",
        display: "flex",
        flexDirection: "column",
        gap: 6,
      }}
    >
      {/* Header */}
      <div
        style={{
          display: "flex",
          alignItems: "center",
          gap: 10,
          flexWrap: "wrap",
        }}
      >
        <span
          style={{
            fontWeight: 700,
            color: tc.accent,
            fontSize: 12,
            fontFamily: '"JetBrains Mono", monospace',
          }}
        >
          #{trace.iteration}
        </span>
        <span style={{ color: tc.text, fontSize: 12 }}>
          {trace.provider}/{trace.model}
        </span>
        <span style={{ color: tc.textMuted, fontSize: 11, marginLeft: "auto" }}>
          {formatTimestamp(trace.timestamp ?? "")}
        </span>
      </div>

      {/* Stats row */}
      <div style={{ display: "flex", gap: 8, flexWrap: "wrap", alignItems: "center" }}>
        <span
          style={{
            fontSize: 11,
            color: tc.textSecondary ?? tc.textMuted,
            background: tc.bg,
            border: `1px solid ${tc.border}`,
            borderRadius: 4,
            padding: "1px 6px",
          }}
        >
          {trace.messagesSent} messaggi
        </span>
        <span
          style={{
            fontSize: 11,
            color: tc.textSecondary ?? tc.textMuted,
            background: tc.bg,
            border: `1px solid ${tc.border}`,
            borderRadius: 4,
            padding: "1px 6px",
          }}
        >
          {trace.toolsCount} strumenti
        </span>
        {((trace.inputTokens ?? 0) > 0 || (trace.outputTokens ?? 0) > 0) && (() => {
          const cost = calcCost(trace.model, trace.inputTokens ?? 0, trace.outputTokens ?? 0, trace.cacheReadTokens ?? 0);
          return (
            <>
              <span
                style={{
                  fontSize: 11,
                  color: tc.accent,
                  background: tc.bg,
                  border: `1px solid ${tc.accent}44`,
                  borderRadius: 4,
                  padding: "1px 6px",
                  fontFamily: '"JetBrains Mono", monospace',
                }}
                title={`Ingresso: ${trace.inputTokens ?? 0} tok | Uscita: ${trace.outputTokens ?? 0} tok${(trace.cacheReadTokens ?? 0) > 0 ? ` | Letture cache: ${trace.cacheReadTokens} tok` : ""}${cost != null ? ` | Costo stimato: ${formatCost(cost)}` : ""}`}
              >
                ↑{trace.inputTokens ?? 0} ↓{trace.outputTokens ?? 0}
                {(trace.cacheReadTokens ?? 0) > 0 && <span style={{ color: tc.success, marginLeft: 4 }}>⚡{trace.cacheReadTokens}</span>}
              </span>
              {cost != null && (
                <span
                  style={{
                    fontSize: 11,
                    color: cost > 0.05 ? tc.error : cost > 0.01 ? "#f97316" : tc.textMuted,
                    background: tc.bg,
                    border: `1px solid ${tc.border}`,
                    borderRadius: 4,
                    padding: "1px 6px",
                    fontFamily: '"JetBrains Mono", monospace',
                  }}
                  title="Costo stimato (prezzi pubblici del provider)"
                >
                  {formatCost(cost)}
                </span>
              )}
            </>
          );
        })()}
        <span
          style={{
            fontSize: 11,
            color: stopReasonColor(trace.stopReason ?? "", tc),
            background: tc.bg,
            border: `1px solid ${stopReasonColor(trace.stopReason ?? "", tc)}`,
            borderRadius: 4,
            padding: "1px 6px",
            fontWeight: 600,
          }}
        >
          {trace.stopReason}
        </span>
      </div>

      {/* Response preview — rendered as Markdown */}
      {safeText.trim().length > 0 && (
        <div
          style={{
            fontSize: 12,
            color: tc.text,
            background: tc.bg,
            border: `1px solid ${tc.border}`,
            borderRadius: 4,
            padding: "6px 8px",
            wordBreak: "break-word",
          }}
        >
          <div className="trace-md" style={{ lineHeight: 1.6 }}>
            <ReactMarkdown
              components={{
                p: ({ children }) => (
                  <p style={{ margin: "4px 0" }}>{children}</p>
                ),
                // In react-markdown v9, block code is wrapped in <pre>; code alone = inline
                // eslint-disable-next-line @typescript-eslint/no-explicit-any
                code: (({ children }: any) => (
                  <code
                    style={{
                      fontFamily: '"JetBrains Mono", monospace',
                      fontSize: 11,
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
                      margin: "6px 0",
                      background: tc.bg,
                      border: `1px solid ${tc.border}`,
                      borderRadius: 4,
                      padding: "6px 8px",
                      whiteSpace: "pre-wrap",
                      overflowX: "auto",
                      fontFamily: '"JetBrains Mono", monospace',
                      fontSize: 11,
                      color: tc.text,
                    }}
                  >
                    {children}
                  </pre>
                // eslint-disable-next-line @typescript-eslint/no-explicit-any
                )) as any,
                h1: ({ children }) => (
                  <h1 style={{ fontSize: 14, fontWeight: 700, margin: "8px 0 4px", color: tc.text }}>{children}</h1>
                ),
                h2: ({ children }) => (
                  <h2 style={{ fontSize: 13, fontWeight: 700, margin: "8px 0 4px", color: tc.text }}>{children}</h2>
                ),
                h3: ({ children }) => (
                  <h3 style={{ fontSize: 12, fontWeight: 700, margin: "6px 0 2px", color: tc.text }}>{children}</h3>
                ),
                ul: ({ children }) => (
                  <ul style={{ margin: "4px 0", paddingLeft: 16 }}>{children}</ul>
                ),
                ol: ({ children }) => (
                  <ol style={{ margin: "4px 0", paddingLeft: 16 }}>{children}</ol>
                ),
                li: ({ children }) => (
                  <li style={{ margin: "2px 0" }}>{children}</li>
                ),
                strong: ({ children }) => (
                  <strong style={{ fontWeight: 700, color: tc.text }}>{children}</strong>
                ),
                em: ({ children }) => (
                  <em style={{ fontStyle: "italic" }}>{children}</em>
                ),
                blockquote: ({ children }) => (
                  <blockquote
                    style={{
                      borderLeft: `3px solid ${tc.accent}`,
                      margin: "4px 0",
                      paddingLeft: 8,
                      color: tc.textMuted,
                    }}
                  >
                    {children}
                  </blockquote>
                ),
              }}
            >
              {expanded ? safeText : preview}
            </ReactMarkdown>
          </div>
          {hasMore && (
            <button
              onClick={() => setExpanded((v) => !v)}
              style={{
                background: "none",
                border: "none",
                color: tc.accent,
                cursor: "pointer",
                fontSize: 11,
                padding: "2px 4px",
                marginTop: 2,
              }}
            >
              {expanded ? "meno ▲" : "... altro ▼"}
            </button>
          )}
        </div>
      )}

      {/* Tool calls */}
      {(trace.toolCalls ?? []).length > 0 && (
        <div style={{ display: "flex", flexDirection: "column", gap: 4 }}>
          {(trace.toolCalls ?? []).map((tc_call, i) => (
            <div
              key={i}
              style={{
                fontSize: 11,
                color: tc.textMuted,
                fontFamily: '"JetBrains Mono", monospace',
                background: tc.bg,
                border: `1px solid ${tc.border}`,
                borderRadius: 4,
                padding: "4px 8px",
              }}
            >
              <span className="font-semibold text-accent">{tc_call.name}</span>
              {" "}
              <span>
                {JSON.stringify(tc_call.input).slice(0, 120)}
                {JSON.stringify(tc_call.input).length > 120 ? "…" : ""}
              </span>
            </div>
          ))}
        </div>
      )}
    </div>
  );
}

function isGroupableTrace(trace: AITraceEvent): boolean {
  const reason = (trace.stopReason ?? "").toLowerCase();
  const hasText = humanizeTraceText(trace.responseText).trim().length > 0;
  return reason === "tool_use" && !hasText;
}

type TraceItem =
  | { type: "single"; trace: AITraceEvent; key: string }
  | { type: "group"; traces: AITraceEvent[]; key: string };

function buildTraceItems(traces: AITraceEvent[]): TraceItem[] {
  const items: TraceItem[] = [];
  let i = 0;
  while (i < traces.length) {
    const t = traces[i];
    if (isGroupableTrace(t)) {
      const group: AITraceEvent[] = [t];
      let j = i + 1;
      while (j < traces.length && traces[j].runId === t.runId && isGroupableTrace(traces[j])) {
        group.push(traces[j]);
        j++;
      }
      if (group.length > 1) {
        items.push({ type: "group", traces: group, key: `grp-${t.runId}-${t.iteration}` });
        i = j;
      } else {
        items.push({ type: "single", trace: t, key: `${t.runId}-${t.iteration}-${i}` });
        i++;
      }
    } else {
      items.push({ type: "single", trace: t, key: `${t.runId}-${t.iteration}-${i}` });
      i++;
    }
  }
  return items;
}

function TraceGroupCard({
  traces,
  tc,
}: {
  traces: AITraceEvent[];
  tc: ReturnType<typeof useThemeColors>;
}) {
  const [expanded, setExpanded] = useState(false);
  const first = traces[0];
  const last = traces[traces.length - 1];
  const totalIn = traces.reduce((s, t) => s + (t.inputTokens ?? 0), 0);
  const totalOut = traces.reduce((s, t) => s + (t.outputTokens ?? 0), 0);
  const totalCacheRead = traces.reduce((s, t) => s + (t.cacheReadTokens ?? 0), 0);
  const cost = calcCost(first.model, totalIn, totalOut, totalCacheRead);

  return (
    <div
      style={{
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
          padding: "7px 12px",
          background: "none",
          border: "none",
          cursor: "pointer",
          textAlign: "left",
          flexWrap: "wrap",
        }}
      >
        <span style={{ fontSize: 10, color: tc.textMuted }}>{expanded ? "▲" : "▼"}</span>
        <span style={{ fontWeight: 600, color: tc.accent, fontSize: 12 }}>
          #{first.iteration}–#{last.iteration}
        </span>
        <span style={{ color: tc.text, fontSize: 12 }}>
          {first.provider}/{first.model}
        </span>
        <span
          style={{
            fontSize: 11,
            color: tc.textSecondary,
            background: tc.bg,
            border: `1px solid ${tc.border}`,
            borderRadius: 4,
            padding: "1px 6px",
          }}
        >
          {traces.length} chiamate tool_use
        </span>
        <span
          style={{
            fontSize: 11,
            color: tc.accent,
            background: tc.bg,
            border: `1px solid ${tc.accent}44`,
            borderRadius: 4,
            padding: "1px 6px",
            fontFamily: '"JetBrains Mono", monospace',
          }}
        >
          ↑{totalIn.toLocaleString("it-IT")} ↓{totalOut.toLocaleString("it-IT")}
        </span>
        {cost != null && (
          <span
            style={{
              fontSize: 11,
              color: cost > 0.05 ? tc.error : cost > 0.01 ? "#f97316" : tc.textMuted,
              fontFamily: '"JetBrains Mono", monospace',
            }}
          >
            {formatCost(cost)}
          </span>
        )}
        <span style={{ marginLeft: "auto", fontSize: 11, color: tc.textMuted }}>
          {formatTimestamp(last.timestamp ?? "")}
        </span>
      </button>
      {expanded && (
        <div
          style={{
            borderTop: `1px solid ${tc.border}`,
            display: "flex",
            flexDirection: "column",
            gap: 6,
            padding: "8px 10px",
            maxHeight: 300,
            overflowY: "auto",
          }}
        >
          {traces.map((trace, i) => (
            <TraceCard key={`${trace.runId}-${trace.iteration}-${i}`} trace={trace} tc={tc} />
          ))}
        </div>
      )}
    </div>
  );
}

export function AITracePanel({ traces, onClear }: AITracePanelProps) {
  const tc = useThemeColors();
  const bottomRef = useRef<HTMLDivElement>(null);

  useEffect(() => {
    bottomRef.current?.scrollIntoView({ behavior: "smooth" });
  }, [traces]);

  return (
    <div style={{ display: "flex", flexDirection: "column", height: "100%", minHeight: 0 }}>
      {/* Toolbar */}
      {traces.length > 0 && (
        <div
          style={{
            display: "flex",
            justifyContent: "flex-end",
            padding: "4px 8px",
            borderBottom: `1px solid ${tc.border}`,
            flexShrink: 0,
          }}
        >
          <button
            onClick={onClear}
            style={{
              background: "none",
              border: `1px solid ${tc.border}`,
              borderRadius: 4,
              color: tc.textMuted,
              cursor: "pointer",
              padding: "2px 8px",
              fontSize: 11,
            }}
          >
            Pulisci
          </button>
        </div>
      )}

      {/* Content */}
      <div
        style={{
          flex: 1,
          minHeight: 0,
          overflow: "auto",
          padding: 10,
          display: "flex",
          flexDirection: "column",
          gap: 8,
        }}
      >
        {traces.length === 0 ? (
          <div style={{ color: tc.textMuted, fontSize: 13 }}>
            Nessuna chiamata AI ancora
          </div>
        ) : (() => {
          const items = buildTraceItems(traces);
          const elements: React.ReactNode[] = [];
          let lastRunId: string | null = null;

          items.forEach((item) => {
            const runId = item.type === "single" ? item.trace.runId : item.traces[0].runId;
            if (runId !== lastRunId) {
              if (lastRunId !== null) {
                elements.push(
                  <div key={`sep-${runId}`} style={{
                    borderTop: `1px solid ${tc.border}`, margin: "4px 0",
                    display: "flex", alignItems: "center", gap: 6,
                  }}>
                    <span style={{ fontSize: 10, color: tc.textMuted, whiteSpace: "nowrap" }}>
                      Run {runId.slice(0, 8)}…
                    </span>
                    <div style={{ flex: 1, height: 1, background: tc.border }} />
                  </div>
                );
              }
              lastRunId = runId;
            }

            if (item.type === "group") {
              elements.push(
                <TraceGroupCard key={item.key} traces={item.traces} tc={tc} />
              );
            } else {
              elements.push(
                <TraceCard key={item.key} trace={item.trace} tc={tc} />
              );
            }
          });
          return elements;
        })()}
        <div ref={bottomRef} />
      </div>
    </div>
  );
}
