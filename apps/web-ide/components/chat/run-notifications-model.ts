// Modello (dati) del centro notifiche del run (ADR 0037). Separato dalla vista
// (run-notifications.tsx) perche' e' logica PURA su segnali strutturati (regola
// M) e va testato senza JSX (regola O: `node --test` striscia i tipi ma non il
// JSX). La vista importa questi tipi + `deriveRunNotifications` da qui.
//
// Punto unico (regola L): "quali eventi del nastro sono salienti, con quale
// gravita', categoria e ancora" vive SOLO in questa funzione. Gli import sono
// tutti file .ts puri (providerBaseColor, toolLabel, switchCauseLabel), mai
// componenti JSX.

import { providerBaseColor } from "./provider-icon-logic";
import { toolLabel } from "./tool-labels";
import { switchCauseLabel } from "../../lib/use-chat/activity-stream";
import type { Theme } from "../../lib/theme";
import type { ActivityStream } from "../../lib/use-chat/activity-stream";

type ThemeColors = Theme;

export type RunNotificationTone = "info" | "warn" | "block";

/** Categoria STRUTTURATA della notifica, assegnata ALLA FONTE dal discriminante
 *  dell'evento (regola M: mai dedotta dal titolo/testo). Distingue i tipi senza
 *  costringere i consumatori a matchare la stringa `title`. */
export type RunNotificationKind =
  | "tool_error"
  | "switch"
  | "context_overflow"
  | "council"
  | "panel"
  | "run_status";

/** Riferimento posizionale all'evento sorgente nel nastro, per le fasi
 *  successive (aprire il file fallito, leggere il parere, ecc.): risale a
 *  stream.segments[segIndex].events[evIndex]. `evIndex` assente per le notifiche
 *  a livello segmento (cambio provider). */
export interface RunNotificationSource {
  segIndex: number;
  evIndex?: number;
}

export interface RunNotification {
  tone: RunNotificationTone;
  /** Categoria strutturata (dal discriminante evento, regola M). */
  kind: RunNotificationKind;
  title: string;
  detail?: string;
  color: string;
  /** Ancora locale dell'evento nel nastro (da ev.anchorId): deep-link alla riga
   *  esatta. Assente per le notifiche di stato run senza riga nel nastro. */
  anchorId?: string;
  /** Ancora locale del segmento (da seg.anchorId): fallback quando l'evento e'
   *  stato cappato dalla vista live (il segmento resta nel DOM, l'evento no). */
  segmentAnchorId?: string;
  /** Riferimento posizionale all'evento sorgente (fasi successive). */
  source?: RunNotificationSource;
  /** Numero di occorrenze collassate in questa voce (dedup per chiave
   *  strutturata, regola M). Assente/1 = voce singola; > 1 -> la vista mostra
   *  "xN". La voce conserva l'ancora della PRIMA occorrenza. */
  count?: number;
}

/** Stati del run che richiedono l'attenzione bloccante dell'utente. */
export const BLOCKING_STATUSES: ReadonlySet<string> = new Set([
  "awaiting_confirmation",
  "blocked_needs_input",
]);

/** Deriva le notifiche salienti dal nastro + stato run (segnali strutturati).
 *  Ogni notifica porta la categoria `kind` alla fonte (regola M) e, dove esiste
 *  una riga nel nastro, l'ancora dell'evento + del segmento (lette dagli oggetti
 *  dello stream, valori canonici assegnati da composeActivityStream: la campanella
 *  NON ricalcola l'ancora, la LEGGE — cosi' coincide col DOM del renderer).
 *
 *  Deduplica (regola M): le categorie ripetibili (passo fallito, cambio provider,
 *  contesto oltre il limite) collassano per una CHIAVE STRUTTURATA costruita dai
 *  campi dell'evento — mai dalla stringa `detail` gia' appiattita — cosi' N
 *  fallimenti sullo STESSO target diventano una voce "xN" invece di N voci, e il
 *  contatore della campanella non esplode sui run lunghi. Consiglio/panel/stato
 *  non si deduplicano (istanze distinte: es. convening -> complete). */
export function deriveRunNotifications(
  stream: ActivityStream,
  runStatus: string | undefined,
  tc: ThemeColors,
): RunNotification[] {
  // Map = ordine di prima-occorrenza preservato; `count` accumulato sul primo.
  const byKey = new Map<string, RunNotification>();
  let uniqueSeq = 0;
  const add = (n: RunNotification, dedupKey: string | null): void => {
    // `null` = notifica NON deduplicabile: chiave unica per non fonderla.
    const key = dedupKey ?? `u${uniqueSeq++}`;
    const existing = byKey.get(key);
    if (existing) {
      existing.count = (existing.count ?? 1) + 1;
    } else {
      byKey.set(key, n);
    }
  };

  for (let si = 0; si < stream.segments.length; si++) {
    const seg = stream.segments[si];
    // Cambio provider = evento saliente (a livello segmento: la banda switch).
    if (seg.openedBySwitch && seg.switch) {
      const sw = seg.switch;
      // Causa STRUTTURATA onesta (regola M): l'etichetta del vocabolario chiuso
      // (switchCauseLabel), mai il codice grezzo di `reason`. Fallback pulito a
      // reason quando la causa non e' nota (arricchimento backend opzionale).
      const causeText = switchCauseLabel(sw.cause) ?? sw.reason;
      add(
        {
          tone: "warn",
          kind: "switch",
          title: "Cambio provider",
          detail: `${sw.fromProvider ?? "?"} -> ${sw.toProvider}${
            causeText ? ` (${causeText})` : ""
          }`,
          color: providerBaseColor(sw.toProvider),
          anchorId: seg.anchorId,
          segmentAnchorId: seg.anchorId,
          source: { segIndex: si },
        },
        `switch|${sw.fromProvider ?? ""}|${sw.toProvider}|${sw.cause ?? ""}`,
      );
    }
    // Step fallito = evento saliente.
    for (let ei = 0; ei < seg.events.length; ei++) {
      const ev = seg.events[ei];
      if (ev.type === "tool" && ev.outcome === "err") {
        // Il TARGET (path/comando/url) e' il pezzo chiave finora mancante: senza
        // di esso la voce non dice QUALE file/comando e' fallito. Entra sia nel
        // display sia nella chiave di dedup (voci su target diversi restano
        // distinte; ripetizioni sullo stesso target collassano con contatore).
        add(
          {
            tone: "warn",
            kind: "tool_error",
            title: "Passo fallito",
            detail: `${toolLabel(ev.name)}${ev.target ? ` — ${ev.target}` : ""}${
              typeof ev.exitCode === "number" ? ` (exit ${ev.exitCode})` : ""
            }`,
            color: tc.error,
            anchorId: ev.anchorId,
            segmentAnchorId: seg.anchorId,
            source: { segIndex: si, evIndex: ei },
          },
          `tool|${ev.name}|${ev.target ?? ""}|${ev.exitCode ?? ""}`,
        );
      }
      if (ev.type === "context_overflow") {
        add(
          {
            tone: "warn",
            kind: "context_overflow",
            title: "Contesto oltre il limite",
            detail: ev.detail,
            color: tc.error,
            anchorId: ev.anchorId,
            segmentAnchorId: seg.anchorId,
            source: { segIndex: si, evIndex: ei },
          },
          `ctx|${ev.provider ?? ""}`,
        );
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
        add(
          {
            tone: ev.phase === "convening" ? "info" : ev.degraded ? "warn" : "info",
            kind: "council",
            title: "Consiglio delle Competenze",
            detail:
              figureDetail ??
              (ev.degraded
                ? ev.degradationReason ??
                  "Gate attivato ma la convocazione non ha prodotto una sintesi valida."
                : "Attivato dall'analisi agentica/deterministica della richiesta."),
            color: ev.degraded ? "#f59e0b" : "#0ea5e9",
            anchorId: ev.anchorId,
            segmentAnchorId: seg.anchorId,
            source: { segIndex: si, evIndex: ei },
          },
          null,
        );
      }
      if (ev.type === "multi_provider_panel") {
        add(
          {
            tone: ev.degraded ? "warn" : "info",
            kind: "panel",
            title: ev.productName,
            detail: ev.degraded
              ? ev.degradationReason ??
                "Provider distinti insufficienti: panel multi-provider non convocato."
              : typeof ev.providerCount === "number" && ev.providerCount > 0
                ? `${ev.providerCount} provider distinti hanno analizzato la richiesta.`
                : "Analisi parallela su provider/modelli distinti.",
            color: ev.degraded ? "#f59e0b" : "#6366f1",
            anchorId: ev.anchorId,
            segmentAnchorId: seg.anchorId,
            source: { segIndex: si, evIndex: ei },
          },
          null,
        );
      }
    }
  }

  if (runStatus && BLOCKING_STATUSES.has(runStatus)) {
    add(
      {
        tone: "block",
        kind: "run_status",
        title:
          runStatus === "awaiting_confirmation" ? "Attesa conferma" : "Attesa input",
        detail: "Il run e' in pausa e richiede la tua azione.",
        color: "#8b5cf6",
      },
      null,
    );
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
    add(
      {
        tone: "info",
        kind: "run_status",
        title: "In attesa dei sub-agent",
        detail:
          typeof count === "number" && count > 0
            ? `${count} sub-agent in background, il run riprende al loro completamento.`
            : "Il run riprende al completamento dei sub-agent in background.",
        color: "#8b5cf6",
      },
      null,
    );
  }

  return Array.from(byKey.values());
}

/** true se tra le notifiche c'e' almeno un evento bloccante. */
export function hasBlocking(notifications: RunNotification[]): boolean {
  return notifications.some((n) => n.tone === "block");
}
