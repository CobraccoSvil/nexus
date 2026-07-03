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

import { useEffect, useMemo, useRef, useState } from "react";
import { useThemeColors } from "../../lib/theme";
import { providerBaseColor } from "./provider-badge";
import { toolLabel } from "./tool-labels";
import type { ActivityStream } from "../../lib/use-chat/activity-stream";

type ThemeColors = ReturnType<typeof useThemeColors>;

export type RunNotificationTone = "info" | "warn" | "block";

export interface RunNotification {
  tone: RunNotificationTone;
  title: string;
  detail?: string;
  color: string;
}

/** Stati del run che richiedono l'attenzione bloccante dell'utente. */
const BLOCKING_STATUSES: ReadonlySet<string> = new Set([
  "awaiting_confirmation",
  "blocked_needs_input",
]);

/** Deriva le notifiche salienti dal nastro + stato run (segnali strutturati). */
export function deriveRunNotifications(
  stream: ActivityStream,
  runStatus: string | undefined,
  tc: ThemeColors,
): RunNotification[] {
  const out: RunNotification[] = [];

  for (const seg of stream.segments) {
    // Cambio provider = evento saliente.
    if (seg.openedBySwitch && seg.switch) {
      out.push({
        tone: "warn",
        title: "Cambio provider",
        detail: `${seg.switch.fromProvider ?? "?"} -> ${seg.switch.toProvider}${
          seg.switch.reason ? ` (${seg.switch.reason})` : ""
        }`,
        color: providerBaseColor(seg.switch.toProvider),
      });
    }
    // Step fallito = evento saliente.
    for (const ev of seg.events) {
      if (ev.type === "tool" && ev.outcome === "err") {
        out.push({
          tone: "warn",
          title: "Passo fallito",
          detail: `${toolLabel(ev.name)}${typeof ev.exitCode === "number" ? ` (exit ${ev.exitCode})` : ""}`,
          color: tc.error,
        });
      }
      if (ev.type === "context_overflow") {
        out.push({
          tone: "warn",
          title: "Contesto oltre il limite",
          detail: ev.detail,
          color: tc.error,
        });
      }
    }
  }

  if (runStatus && BLOCKING_STATUSES.has(runStatus)) {
    out.push({
      tone: "block",
      title:
        runStatus === "awaiting_confirmation" ? "Attesa conferma" : "Attesa input",
      detail: "Il run e' in pausa e richiede la tua azione.",
      color: "#8b5cf6",
    });
  }

  return out;
}

/** true se tra le notifiche c'e' almeno un evento bloccante. */
function hasBlocking(notifications: RunNotification[]): boolean {
  return notifications.some((n) => n.tone === "block");
}

export function RunNotifications({
  stream,
  runStatus,
  tc,
}: {
  stream: ActivityStream;
  runStatus?: string;
  tc: ThemeColors;
}) {
  const notifications = useMemo(
    () => deriveRunNotifications(stream, runStatus, tc),
    [stream, runStatus, tc],
  );
  const [open, setOpen] = useState(false);
  // Traccia se abbiamo gia' auto-aperto per l'attuale ondata di blocco, cosi'
  // non riapriamo dopo che l'utente ha chiuso.
  const autoOpenedRef = useRef(false);
  const blocking = hasBlocking(notifications);

  // Auto-apertura SOLO su evento bloccante (una volta per transizione a blocco).
  useEffect(() => {
    if (blocking && !autoOpenedRef.current) {
      autoOpenedRef.current = true;
      setOpen(true);
    }
    if (!blocking) autoOpenedRef.current = false;
  }, [blocking]);

  if (notifications.length === 0) return null;

  const count = notifications.length;
  const badgeColor = blocking ? "#8b5cf6" : tc.error;

  return (
    <div style={{ position: "relative", display: "inline-block" }}>
      <button
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
          border: `1px solid ${tc.border}`,
          background: tc.bgCard,
          color: tc.textSecondary,
          display: "inline-flex",
          alignItems: "center",
          justifyContent: "center",
          cursor: "pointer",
          fontSize: 14,
          // Pulsazione discreta quando ci sono notifiche non lette (mai su
          // prefers-reduced-motion, gestito in globals.css se necessario).
          animation: blocking ? "none" : undefined,
          flexShrink: 0,
        }}
      >
        {/* Glifo campanella non-emoji */}
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
          role="dialog"
          aria-label="Notifiche del run"
          style={{
            position: "absolute",
            top: 34,
            right: 0,
            zIndex: 50,
            width: 260,
            maxWidth: "80vw",
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
              fontSize: 10,
              textTransform: "uppercase",
              letterSpacing: "0.08em",
              color: tc.textMuted,
              fontWeight: 700,
              padding: "2px 6px",
            }}
          >
            Notifiche del run
          </div>
          {notifications.map((n, i) => (
            <div
              key={`notif-${i}`}
              style={{
                display: "flex",
                alignItems: "flex-start",
                gap: 8,
                padding: "5px 6px",
                borderRadius: 8,
                background: withAlpha(n.color, 0.08),
                border: `1px solid ${withAlpha(n.color, 0.3)}`,
                minWidth: 0,
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
            </div>
          ))}
        </div>
      )}
    </div>
  );
}

function withAlpha(hex: string, alpha: number): string {
  const m = /^#?([0-9a-fA-F]{6})$/.exec(hex);
  if (!m) return hex;
  const v = m[1];
  const r = parseInt(v.slice(0, 2), 16);
  const g = parseInt(v.slice(2, 4), 16);
  const b = parseInt(v.slice(4, 6), 16);
  return `rgba(${r},${g},${b},${alpha})`;
}
