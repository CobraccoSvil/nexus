"use client";

// Centro notifiche del run (ADR 0037): campanella con contatore + pannello che
// raccoglie gli eventi salienti del turno (cambio provider, step fallito, attesa
// conferma). Auto-apertura SOLO per eventi bloccanti (awaiting_confirmation /
// blocked_needs_input): per gli altri solo badge + pulsazione, senza rubare il
// focus all'utente.
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
import {
  deriveRunNotifications,
  hasBlocking,
  type RunNotification,
} from "./run-notifications-model";
import type { ActivityStream } from "../../lib/use-chat/activity-stream";

type ThemeColors = ReturnType<typeof useThemeColors>;

// Il modello dati (tipi + deriveRunNotifications + hasBlocking + BLOCKING_STATUSES)
// vive nel modulo PURO ./run-notifications-model (regola L: modello vs vista,
// regola O: testabile con `node --test` senza JSX). Qui resta la sola vista.
export type { RunNotification } from "./run-notifications-model";

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
  pendingActions?: Array<{ description: string }>;
  onConfirm?: (runId: string, approved: boolean) => void;
  isConfirming?: boolean;
  tc: ThemeColors;
}) {
  const notifications = useMemo(
    () => deriveRunNotifications(stream, runStatus, tc),
    [stream, runStatus, tc],
  );
  const [open, setOpen] = useState(false);
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
  // accetta piu' ref. Delegando, il pannello guadagna anche la chiusura con
  // Escape, che prima non aveva.
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

  if (notifications.length === 0) return null;

  const count = notifications.length;
  const badgeColor = blocking ? "#8b5cf6" : notifications[0]?.color ?? tc.error;

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
            width: 280,
            maxWidth: "85vw",
            // Drop-up (bottom): la lista cresce nello spazio libero SOPRA la
            // campanella, ancorata in fondo alla chat. Il maxHeight relativo al
            // viewport + overflowY:auto evita che le voci extra sforino sotto il
            // bordo: quando eccedono, il pannello scrolla internamente invece di
            // nascondere le notifiche fuori dal viewport.
            maxHeight: "min(60vh, 360px)",
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
            // Cliccabile solo se la voce ha un'ancora nel nastro e conosciamo il
            // run: le voci di stato senza riga (attesa conferma, sub-agent)
            // restano statiche (degrado pulito).
            const clickable = !!runId && !!n.anchorId;
            const boxStyle: React.CSSProperties = {
              display: "flex",
              alignItems: "flex-start",
              gap: 8,
              padding: "5px 6px",
              borderRadius: 8,
              background: withAlpha(n.color, 0.08),
              border: `1px solid ${withAlpha(n.color, 0.3)}`,
              minWidth: 0,
            };
            const inner = (
              <>
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
                <div style={{ minWidth: 0 }}>
                  <div style={{ fontSize: 11.5, fontWeight: 600, color: tc.text }}>{n.title}</div>
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
              </>
            );
            return clickable ? (
              <button
                key={`notif-${i}`}
                type="button"
                onClick={() => handleNotificationClick(n)}
                title="Vai al punto nel nastro"
                style={{
                  ...boxStyle,
                  width: "100%",
                  textAlign: "left",
                  font: "inherit",
                  cursor: "pointer",
                }}
              >
                {inner}
              </button>
            ) : (
              <div key={`notif-${i}`} style={boxStyle}>
                {inner}
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
                gap: 6,
              }}
            >
              <div style={{ fontSize: 10.5, fontWeight: 600, color: tc.textSecondary }}>
                Azioni in attesa:
              </div>
              {pendingActions!.map((action, idx) => (
                <div
                  key={`pending-${idx}`}
                  style={{
                    fontFamily: "var(--font-mono)",
                    fontSize: 10,
                    color: tc.text,
                    background: `${tc.border}30`,
                    borderRadius: 4,
                    padding: "3px 6px",
                    wordBreak: "break-word",
                  }}
                >
                  {action.description}
                </div>
              ))}
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
