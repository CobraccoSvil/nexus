"use client";

import { useEffect, useRef, useState, useCallback } from "react";
import type { RefObject } from "react";
import type { ChatMessage, AgentRunInfo, AgentStep } from "../../lib/api-client";
import { getAgentRun } from "../../lib/api-client";
import type { useThemeColors } from "../../lib/theme";
import { MarkdownBlock } from "./markdown-renderer";

type ThemeColors = ReturnType<typeof useThemeColors>;

function parseThinking(content: string): { thinking: string | null; text: string } {
  const match = /^<nexus:thinking>([\s\S]*?)<\/nexus:thinking>\n*/s.exec(content);
  if (!match) return { thinking: null, text: content };
  return { thinking: match[1].trim(), text: content.slice(match[0].length) };
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

function isRunSummaryMessage(msg: ChatMessage): boolean {
  if (msg.role !== "assistant") return false;
  if (msg.totalTokens && msg.totalTokens > 0) return true;
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
            gap: 4,
            maxHeight: 220,
            overflowY: "auto",
          }}
        >
          {messages.map((m) => {
            const { tokens, model } = getRunInfo(m);
            return (
              <div key={m.id} style={{ fontSize: 11, color: tc.textMuted, display: "flex", gap: 6 }}>
                <span style={{ color: tc.textSecondary }}>{tokens.toLocaleString("it-IT")} tok</span>
                {model && <span>· {model}</span>}
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

function InlineTruncated({ text, maxLen = 400, tc, mono = true }: { text: string; maxLen?: number; tc: ThemeColors; mono?: boolean }) {
  const [full, setFull] = useState(false);
  const truncated = text.length > maxLen;
  const display = full || !truncated ? text : text.slice(0, maxLen) + "...";
  return (
    <div>
      <pre style={{
        fontFamily: mono ? "monospace" : "inherit",
        fontSize: 11,
        whiteSpace: "pre-wrap",
        wordBreak: "break-word",
        margin: 0,
        maxHeight: full ? 500 : 160,
        overflowY: "auto",
        color: tc.text,
        background: `${tc.bgInput ?? tc.border}40`,
        borderRadius: 4,
        padding: "4px 6px",
      }}>
        {display}
      </pre>
      {truncated && (
        <button
          type="button"
          onClick={() => setFull(v => !v)}
          style={{ fontSize: 10, color: tc.accent, background: "none", border: "none", cursor: "pointer", padding: "2px 0", fontWeight: 600 }}
        >
          {full ? "Comprimi" : `Mostra tutto (${text.length.toLocaleString()} car.)`}
        </button>
      )}
    </div>
  );
}

function formatStepInput(input: Record<string, unknown>): string {
  const lines: string[] = [];
  for (const [key, val] of Object.entries(input)) {
    if (typeof val === "string" && val.length > 300) {
      lines.push(`${key}: [${val.length} car.]`);
    } else if (typeof val === "object" && val !== null) {
      const j = JSON.stringify(val);
      lines.push(j.length > 300 ? `${key}: [oggetto, ${j.length} car.]` : `${key}: ${j}`);
    } else {
      lines.push(`${key}: ${String(val)}`);
    }
  }
  return lines.join("\n");
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
          {/* Header riepilogo run */}
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
            <StepStatusBadge status={
              runInfo.status === "completed" ? "completed" :
              runInfo.status === "failed" ? "failed" : "running"
            } tc={tc} />
            <span>{steps.length} step</span>
            {runInfo.iterationCount > 0 && <span>{runInfo.iterationCount} iterazioni</span>}
          </div>

          {/* Lista step */}
          <div style={{ padding: "4px 0" }}>
            {steps.map((step) => {
              const isExp = expandedIdx === step.stepIndex;
              const hasInput = step.toolInput && Object.keys(step.toolInput).length > 0;
              const hasResult = Boolean(step.toolResult);
              const clickable = hasInput || hasResult;
              const borderColor =
                step.status === "failed" ? tc.error :
                step.status === "running" ? tc.accent : "#22c55e";

              return (
                <div key={step.stepIndex}>
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
                    <span style={{ minWidth: 22, textAlign: "right", opacity: 0.5, fontSize: 10, fontVariantNumeric: "tabular-nums" }}>
                      {step.stepIndex + 1}.
                    </span>
                    {clickable && (
                      <span style={{ fontSize: 9, opacity: 0.5, minWidth: 10 }}>
                        {isExp ? "▼" : "▶"}
                      </span>
                    )}
                    <span style={{ fontFamily: "monospace", fontSize: 11, color: tc.text }}>
                      {step.toolName}
                    </span>
                    <StepStatusBadge status={step.status} tc={tc} />
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
                        <div style={{ fontSize: 10, opacity: 0.5, fontFamily: "monospace" }}>
                          {new Date(step.createdAt).toLocaleTimeString()}
                        </div>
                      )}
                    </div>
                  )}
                </div>
              );
            })}
          </div>
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

  const grouped = groupMessages(messages);

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
              ) : (() => {
                const { thinking, text } = parseThinking(message.content ?? "");
                return (
                  <>
                    {thinking && <ThinkingPanel thinking={thinking} />}
                    <MarkdownBlock content={text} />
                  </>
                );
              })()}
            </div>

            {/* Pannello step agente (caricamento lazy dal DB) */}
            {!isUser && message.runId && message.automationMode === "agent" && (
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
                <span>{(message.totalTokens ?? 0).toLocaleString("it-IT")} token</span>
                {(message.promptTokens ?? 0) > 0 && (
                  <span>({(message.promptTokens ?? 0).toLocaleString("it-IT")} in / {(message.completionTokens ?? 0).toLocaleString("it-IT")} out)</span>
                )}
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
