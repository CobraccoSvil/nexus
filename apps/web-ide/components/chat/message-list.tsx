"use client";

import { useEffect, useRef, useState } from "react";
import type { RefObject } from "react";
import type { ChatMessage } from "../../lib/api-client";
import type { useThemeColors } from "../../lib/theme";
import { MarkdownBlock } from "./markdown-renderer";

type ThemeColors = ReturnType<typeof useThemeColors>;

function parseThinking(content: string): { thinking: string | null; text: string } {
  const match = /^<nexus:thinking>([\s\S]*?)<\/nexus:thinking>\n*/s.exec(content);
  if (!match) return { thinking: null, text: content };
  return { thinking: match[1].trim(), text: content.slice(match[0].length) };
}

function ThinkingPanel({ thinking, tc }: { thinking: string; tc: ThemeColors }) {
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

export interface MessageListProps {
  messages: ChatMessage[];
  busyByMessage: Record<string, string | undefined>;
  tc: ThemeColors;
  t: (key: string) => string;
  onCopy: (messageId: string, content: string) => Promise<boolean> | boolean;
  onResend: (messageId: string) => void;
  onDelete: (messageId: string) => void;
  onFeedback: (messageId: string, content: string) => void;
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
                    {thinking && <ThinkingPanel thinking={thinking} tc={tc} />}
                    <MarkdownBlock content={text} />
                  </>
                );
              })()}
            </div>

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
