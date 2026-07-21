"use client";

// Centro notifiche del run (ADR 0037): campanella con contatore + pannello che
// raccoglie gli eventi salienti del turno (cambio provider, step fallito, attesa
// conferma). Auto-apertura SOLO per eventi bloccanti (awaiting_confirmation /
// blocked_needs_input): per gli altri solo badge, senza rubare il focus.
//
// Le notifiche derivano dal modello ActivityStream (punto unico) + dallo stato
// del run: nessun parsing di testo (regola M), gli eventi sono gia' strutturati.
//
// Stile: inline + useThemeColors, niente Tailwind, niente emoji nei sorgenti.

import { useCallback, useEffect, useMemo, useRef, useState } from "react";
import { useDismissOnOutside } from "../../hooks/use-dismiss-on-outside";
import { useScrollToAnchor } from "../../hooks/use-scroll-to-anchor";
import { useThemeColors } from "../../lib/theme";
import { withAlpha } from "../../lib/color";
import { toolLabel } from "./tool-labels";
import {
  filePathFromToolInput,
  formatStepInput,
  humanizeToolResult,
} from "./step-detail-logic";
import {
  deriveRunNotifications,
  hasBlocking,
  type RunNotification,
} from "./run-notifications-model";
import type { ActivityStream } from "../../lib/use-chat/activity-stream";
import type { AgentPendingAction } from "../../lib/api/agent";

type ThemeColors = ReturnType<typeof useThemeColors>;

// Il modello dati (tipi + deriveRunNotifications + hasBlocking + BLOCKING_STATUSES)
// vive nel modulo PURO ./run-notifications-model (regola L: modello vs vista,
// regola O: testabile con `node --test` senza JSX). Qui resta la sola vista.
export type { RunNotification } from "./run-notifications-model";

/** Bridge globale gia' esistente (ide-shell.tsx) per aprire un file nell'editor. */
function openFileInEditor(path: string): void {
  window.dispatchEvent(new CustomEvent("nexus:editor:open-file", { detail: { path } }));
}

export function RunNotifications({
  stream,
  runStatus,
  runId,
  pendingActions,
  onConfirm,
  isConfirming,
  tc,
}: {
  stream: ActivityStream;
  runStatus?: string;
  runId?: string;
  pendingActions?: AgentPendingAction[];
  onConfirm?: (runId: string, approved: boolean) => void;
  isConfirming?: boolean;
  tc: ThemeColors;
}) {
  const notifications = useMemo(
    () => deriveRunNotifications(stream, runStatus, tc),
    [stream, runStatus, tc],
  );
  const [open, setOpen] = useState(false);
  // Voci "Passo fallito" espanse (indice nella lista renderizzata): mostrano
  // input strutturato + estratto errore umanizzato (delega a step-detail-logic).
  const [expanded, setExpanded] = useState<Set<number>>(() => new Set());
  const panelRef = useRef<HTMLDivElement>(null);
  const bellRef = useRef<HTMLButtonElement>(null);
  // Traccia se abbiamo gia' auto-aperto per l'attuale ondata di blocco, cosi'
  // non riapriamo dopo che l'utente ha chiuso.
  const autoOpenedRef = useRef(false);
  const blocking = hasBlocking(notifications);
  const showHitlActions =
    runStatus === "awaiting_confirmation" &&
    !!runId &&
    !!onConfirm &&
    (pendingActions?.length ?? 0) > 0;

  // Auto-apertura SOLO su evento bloccante (una volta per transizione a blocco).
  useEffect(() => {
    if (blocking && !autoOpenedRef.current) {
      autoOpenedRef.current = true;
      setOpen(true);
    }
    if (!blocking) autoOpenedRef.current = false;
  }, [blocking]);

  // Chiudi al click fuori dal pannello (non blocca il resto della chat). Le zone
  // sono due — pannello e campanello — ed e' il caso per cui il punto unico
  // accetta piu' ref. Delegando, il pannello guadagna anche la chiusura con Escape.
  const chiudiPannello = useCallback(() => setOpen(false), []);
  useDismissOnOutside(open, [panelRef, bellRef], chiudiPannello);

  // Deep-link: click su una voce -> scroll + flash sulla riga esatta del nastro
  // del run corrente (punto unico use-scroll-to-anchor, regola L). Solo le voci
  // con un'ancora sono cliccabili; le voci di stato senza riga restano statiche.
  const scrollToAnchor = useScrollToAnchor();
  const handleNotificationClick = useCallback(
    (n: RunNotification) => {
      if (!runId || !n.anchorId) return;
      scrollToAnchor(runId, n.anchorId, n.segmentAnchorId, tc.accent);
      setOpen(false);
    },
    [runId, scrollToAnchor, tc.accent],
  );

  const toggleExpand = useCallback((i: number) => {
    setExpanded((prev) => {
      const next = new Set(prev);
      if (next.has(i)) next.delete(i);
      else next.add(i);
      return next;
    });
  }, []);

  // Risale al ToolEvent sorgente di una notifica "Passo fallito" (via il
  // riferimento posizionale `source`) per leggerne input/result/target: campi
  // gia' presenti nello stream, non ri-derivati (regola M).
  const toolEventFor = useCallback(
    (n: RunNotification) => {
      if (n.kind !== "tool_error" || !n.source || n.source.evIndex == null) return undefined;
      const ev = stream.segments[n.source.segIndex]?.events[n.source.evIndex];
      return ev && ev.type === "tool" ? ev : undefined;
    },
    [stream],
  );

  if (notifications.length === 0) return null;

  const count = notifications.length;
  const badgeColor = blocking ? "#8b5cf6" : notifications[0]?.color ?? tc.error;

  const monoBoxStyle: React.CSSProperties = {
    fontFamily: "var(--font-mono)",
    fontSize: 10,
    color: tc.text,
    background: `${tc.border}30`,
    borderRadius: 4,
    padding: "3px 6px",
    whiteSpace: "pre-wrap",
    wordBreak: "break-word",
    maxHeight: 120,
    overflowY: "auto",
  };
  const smallBtnStyle: React.CSSProperties = {
    border: `1px solid ${tc.border}`,
    background: "transparent",
    color: tc.textSecondary,
    borderRadius: 5,
    padding: "2px 7px",
    fontSize: 10,
    fontWeight: 600,
    cursor: "pointer",
  };

  return (
    <div style={{ position: "relative", display: "inline-block", flexShrink: 0 }}>
      <button
        ref={bellRef}
        type="button"
        onClick={() => setOpen((v) => !v)}
        title="Centro notifiche del run"
        aria-label={`Notifiche del run (${count})`}
        aria-expanded={open}
        style={{
          position: "relative",
          width: 28,
          height: 28,
          borderRadius: 8,
          border: `1px solid ${blocking ? "#8b5cf6" : tc.border}`,
          background: blocking ? "#8b5cf611" : tc.bgCard,
          color: blocking ? "#8b5cf6" : tc.textSecondary,
          display: "inline-flex",
          alignItems: "center",
          justifyContent: "center",
          cursor: "pointer",
          fontSize: 14,
          flexShrink: 0,
        }}
      >
        <span aria-hidden style={{ fontFamily: "var(--font-mono)" }}>{"◉"}</span>
        <span
          style={{
            position: "absolute",
            top: -5,
            right: -5,
            minWidth: 15,
            height: 15,
            padding: "0 4px",
            borderRadius: 8,
            background: badgeColor,
            color: "#fff",
            fontSize: 9.5,
            fontWeight: 800,
            display: "grid",
            placeItems: "center",
            fontFamily: "var(--font-mono)",
          }}
        >
          {count}
        </span>
      </button>

      {open && (
        <div
          ref={panelRef}
          role="dialog"
          aria-label="Notifiche del run"
          style={{
            position: "absolute",
            bottom: "calc(100% + 6px)",
            right: 0,
            zIndex: 50,
            width: 300,
            maxWidth: "85vw",
            // Drop-up (bottom): la lista cresce nello spazio libero SOPRA la
            // campanella. maxHeight relativo al viewport + overflowY:auto evita
            // che le voci extra sforino sotto il bordo (scroll interno).
            maxHeight: "min(60vh, 380px)",
            overflowY: "auto",
            overflowX: "hidden",
            borderRadius: 10,
            border: `1px solid ${tc.border}`,
            background: tc.bgCard,
            boxShadow: "0 12px 32px -12px rgba(0,0,0,0.6)",
            padding: 6,
            display: "flex",
            flexDirection: "column",
            gap: 4,
            minWidth: 0,
          }}
        >
          <div
            style={{
              display: "flex",
              alignItems: "center",
              justifyContent: "space-between",
              padding: "2px 6px",
            }}
          >
            <div
              style={{
                fontSize: 10,
                textTransform: "uppercase",
                letterSpacing: "0.08em",
                color: tc.textMuted,
                fontWeight: 700,
              }}
            >
              Notifiche del run
            </div>
            <button
              type="button"
              onClick={() => setOpen(false)}
              aria-label="Chiudi notifiche"
              style={{
                border: "none",
                background: "transparent",
                color: tc.textMuted,
                cursor: "pointer",
                fontSize: 14,
                lineHeight: 1,
                padding: "0 2px",
              }}
            >
              x
            </button>
          </div>

          {notifications.map((n, i) => {
            // Cliccabile (deep-link) solo se la voce ha un'ancora nel nastro e
            // conosciamo il run: le voci di stato senza riga restano statiche.
            const clickable = !!runId && !!n.anchorId;
            const toolEv = toolEventFor(n);
            const hasBody = !!toolEv && (!!toolEv.input || !!toolEv.result);
            const filePath = filePathFromToolInput(toolEv?.input) ?? toolEv?.target;
            const isExpanded = expanded.has(i);

            const activate = () => {
              if (clickable) handleNotificationClick(n);
            };

            return (
              <div
                key={`notif-${i}`}
                style={{
                  display: "flex",
                  flexDirection: "column",
                  gap: 4,
                  padding: "5px 6px",
                  borderRadius: 8,
                  background: withAlpha(n.color, 0.08),
                  border: `1px solid ${withAlpha(n.color, 0.3)}`,
                  minWidth: 0,
                }}
              >
                {/* Header cliccabile per il deep-link (non un <button> per non
                    annidare i controlli dettagli/apri: div con role button). */}
                <div
                  role={clickable ? "button" : undefined}
                  tabIndex={clickable ? 0 : undefined}
                  onClick={activate}
                  onKeyDown={
                    clickable
                      ? (e) => {
                          if (e.key === "Enter" || e.key === " ") {
                            e.preventDefault();
                            activate();
                          }
                        }
                      : undefined
                  }
                  title={clickable ? "Vai al punto nel nastro" : undefined}
                  style={{
                    display: "flex",
                    alignItems: "flex-start",
                    gap: 8,
                    minWidth: 0,
                    cursor: clickable ? "pointer" : "default",
                  }}
                >
                  <span
                    aria-hidden
                    style={{
                      width: 8,
                      height: 8,
                      borderRadius: "50%",
                      background: n.color,
                      marginTop: 5,
                      flexShrink: 0,
                    }}
                  />
                  <div style={{ minWidth: 0, flex: 1 }}>
                    <div
                      style={{
                        fontSize: 11.5,
                        fontWeight: 600,
                        color: tc.text,
                        display: "flex",
                        alignItems: "center",
                        gap: 6,
                      }}
                    >
                      <span>{n.title}</span>
                      {n.count && n.count > 1 && (
                        <span
                          title={`${n.count} occorrenze`}
                          style={{
                            fontSize: 9.5,
                            fontWeight: 700,
                            color: tc.textMuted,
                            background: `${tc.border}55`,
                            borderRadius: 6,
                            padding: "0 5px",
                            fontFamily: "var(--font-mono)",
                          }}
                        >
                          {`x${n.count}`}
                        </span>
                      )}
                    </div>
                    {n.detail && (
                      <div
                        style={{
                          fontSize: 11,
                          color: tc.textMuted,
                          overflow: "hidden",
                          textOverflow: "ellipsis",
                          wordBreak: "break-word",
                        }}
                      >
                        {n.detail}
                      </div>
                    )}
                  </div>
                </div>

                {/* Controlli del passo fallito: espansione dettagli + apri file. */}
                {n.kind === "tool_error" && (hasBody || filePath) && (
                  <div style={{ display: "flex", gap: 6, flexWrap: "wrap" }}>
                    {hasBody && (
                      <button
                        type="button"
                        onClick={() => toggleExpand(i)}
                        aria-expanded={isExpanded}
                        style={smallBtnStyle}
                      >
                        {isExpanded ? "Nascondi dettagli" : "Dettagli"}
                      </button>
                    )}
                    {filePath && (
                      <button
                        type="button"
                        onClick={() => {
                          openFileInEditor(filePath);
                          setOpen(false);
                        }}
                        title={filePath}
                        style={{ ...smallBtnStyle, color: tc.accent, borderColor: tc.accent }}
                      >
                        Apri nell'editor
                      </button>
                    )}
                  </div>
                )}

                {n.kind === "tool_error" && isExpanded && toolEv && (
                  <div style={{ display: "flex", flexDirection: "column", gap: 4 }}>
                    {toolEv.input && (
                      <div style={monoBoxStyle}>{formatStepInput(toolEv.input)}</div>
                    )}
                    {toolEv.result && (
                      <div style={{ ...monoBoxStyle, color: tc.error }}>
                        {humanizeToolResult(toolEv.result).text}
                      </div>
                    )}
                  </div>
                )}
              </div>
            );
          })}

          {showHitlActions && (
            <div
              style={{
                marginTop: 4,
                padding: "6px 6px 4px",
                borderTop: `1px solid ${tc.border}`,
                display: "flex",
                flexDirection: "column",
                gap: 8,
              }}
            >
              <div style={{ fontSize: 10.5, fontWeight: 600, color: tc.textSecondary }}>
                Azioni in attesa:
              </div>
              {pendingActions!.map((action, idx) => {
                // HITL informato: mostra il tool + i parametri ESATTI che verranno
                // eseguiti (delega a toolLabel/formatStepInput, regola L), cosi'
                // l'approvazione e' consapevole. Se il bersaglio e' un file, lo si
                // puo' aprire prima di approvare.
                const path = filePathFromToolInput(action.toolInput);
                return (
                  <div
                    key={`pending-${idx}`}
                    style={{ display: "flex", flexDirection: "column", gap: 3 }}
                  >
                    <div style={{ fontSize: 10.5, fontWeight: 600, color: tc.text }}>
                      {toolLabel(action.toolName)}
                    </div>
                    <div style={monoBoxStyle}>{formatStepInput(action.toolInput)}</div>
                    {path && (
                      <button
                        type="button"
                        onClick={() => openFileInEditor(path)}
                        title={path}
                        style={{ ...smallBtnStyle, alignSelf: "flex-start", color: tc.accent, borderColor: tc.accent }}
                      >
                        Apri il file
                      </button>
                    )}
                  </div>
                );
              })}
              <div style={{ display: "flex", gap: 6 }}>
                <button
                  type="button"
                  disabled={isConfirming}
                  onClick={() => {
                    onConfirm!(runId!, true);
                    setOpen(false);
                  }}
                  style={{
                    flex: 1,
                    padding: "5px 10px",
                    borderRadius: 6,
                    border: "none",
                    background: isConfirming ? "#6b7280" : "#22c55e",
                    color: "#fff",
                    cursor: isConfirming ? "wait" : "pointer",
                    fontWeight: 600,
                    fontSize: 11,
                  }}
                >
                  {isConfirming ? "Conferma..." : "Approva"}
                </button>
                <button
                  type="button"
                  disabled={isConfirming}
                  onClick={() => {
                    onConfirm!(runId!, false);
                    setOpen(false);
                  }}
                  style={{
                    flex: 1,
                    padding: "5px 10px",
                    borderRadius: 6,
                    border: `1px solid ${tc.border}`,
                    background: "transparent",
                    color: tc.error,
                    cursor: isConfirming ? "wait" : "pointer",
                    fontWeight: 600,
                    fontSize: 11,
                  }}
                >
                  Annulla
                </button>
              </div>
            </div>
          )}
        </div>
      )}
    </div>
  );
}
