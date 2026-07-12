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
      if (ev.type === "council_of_competencies") {
        const failedFigures = ev.figureReports?.filter((r) => r.status !== "advisory_ok") ?? [];
        const figureDetail =
          ev.phase === "convening"
            ? typeof ev.completedCount === "number" &&
              typeof ev.figureCount === "number" &&
              ev.figureCount > 0
              ? `${ev.completedCount}/${ev.figureCount} figure completate`
              : "Convocazione figure in corso"
            : ev.degraded && failedFigures.length > 0
              ? failedFigures.map((r) => `${r.kind}: ${r.detail_message}`).join(" · ")
              : undefined;
        out.push({
          tone: ev.phase === "convening" ? "info" : ev.degraded ? "warn" : "info",
          title: "Consiglio delle Competenze",
          detail:
            figureDetail ??
            (ev.degraded
              ? ev.degradationReason ??
                "Gate attivato ma la convocazione non ha prodotto una sintesi valida."
              : "Attivato dall'analisi agentica/deterministica della richiesta."),
          color: ev.degraded ? "#f59e0b" : "#0ea5e9",
        });
      }
      if (ev.type === "multi_provider_panel") {
        out.push({
          tone: ev.degraded ? "warn" : "info",
          title: ev.productName,
          detail: ev.degraded
            ? ev.degradationReason ??
              "Provider distinti insufficienti: panel multi-provider non convocato."
            : typeof ev.providerCount === "number" && ev.providerCount > 0
              ? `${ev.providerCount} provider distinti hanno analizzato la richiesta.`
              : "Analisi parallela su provider/modelli distinti.",
          color: ev.degraded ? "#f59e0b" : "#6366f1",
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

  // Fan-in async: il run e' sospeso in attesa dei sub-agent in background.
  // NON e' bloccante per l'utente (non deve agire, solo attendere): tono "info"
  // e FUORI da BLOCKING_STATUSES cosi' NON auto-apre il pannello rubando il
  // focus. Il conteggio, se noto, arriva dall'evento awaiting_subagents del
  // nastro (segnale strutturato, regola M).
  if (runStatus === "awaiting_subagents") {
    let count: number | undefined;
    for (const seg of stream.segments) {
      for (const ev of seg.events) {
        if (ev.type === "awaiting_subagents" && typeof ev.count === "number") {
          count = ev.count;
        }
      }
    }
    out.push({
      tone: "info",
      title: "In attesa dei sub-agent",
      detail:
        typeof count === "number" && count > 0
          ? `${count} sub-agent in background, il run riprende al loro completamento.`
          : "Il run riprende al completamento dei sub-agent in background.",
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

  // Chiudi al click fuori dal pannello (non blocca il resto della chat).
  useEffect(() => {
    if (!open) return;
    const onPointerDown = (ev: MouseEvent) => {
      const target = ev.target as Node;
      if (panelRef.current?.contains(target)) return;
      if (bellRef.current?.contains(target)) return;
      setOpen(false);
    };
    document.addEventListener("mousedown", onPointerDown);
    return () => document.removeEventListener("mousedown", onPointerDown);
  }, [open]);

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

function withAlpha(hex: string, alpha: number): string {
  const m = /^#?([0-9a-fA-F]{6})$/.exec(hex);
  if (!m) return hex;
  const v = m[1];
  const r = parseInt(v.slice(0, 2), 16);
  const g = parseInt(v.slice(2, 4), 16);
  const b = parseInt(v.slice(4, 6), 16);
  return `rgba(${r},${g},${b},${alpha})`;
}
