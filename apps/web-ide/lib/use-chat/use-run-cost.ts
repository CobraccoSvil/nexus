"use client";

// Il perimetro contabile di UN run per il footer del nastro attivita': dal
// ledger, mai dalle tracce. Il criterio e la vista stanno nel modulo puro
// (use-run-cost-logic.ts); qui c'e' solo il quando si legge.
//
// UNA FONTE, DUE MODI DI ARRIVARCI.
//
//   - Turno IN CORSO: il perimetro e' gia' in memoria. Il contatore sotto la
//     chat lo rilegge al ritmo del run (`refreshSessionUsage`, che passa il
//     `run_id` proprio per averlo), quindi qui non parte nessuna richiesta e il
//     footer non resta fermo al valore dell'istante in cui e' comparso — che e'
//     precisamente cio' che una lettura unica al montaggio produrrebbe su un run
//     ancora vivo.
//   - Turno STORICO: il perimetro e' un altro, e si chiede. Una lettura per
//     footer effettivamente mostrato, non per messaggio in cronologia: il footer
//     storico vive dentro una riga collassata e compare solo all'espansione,
//     quindi il numero di richieste lo governano i click e non la lunghezza
//     della conversazione. E' lo stesso patto di `useResolvedRunSteps`, che
//     differisce allo stesso modo il caricamento degli step storici.
//
// L'alternativa era un endpoint che accetti piu' `run_id` e una lettura sola per
// cronologia: risponderebbe alla stessa domanda per run che nessuno guardera',
// e il costo lo pagherebbe ogni apertura di chat invece delle espansioni.

import { useEffect, useState } from "react";
import { getSessionUsage } from "../api/billing";
import { perimetroGiaNoto, type RipartizioneRun } from "./use-run-cost-logic";
import type { CurrentRunUsage } from "../../components/chat/token-usage-bar-logic";

/**
 * Il perimetro del run: quello gia' noto se e' suo, altrimenti letto una volta.
 *
 * Un errore di lettura NON degrada a «nessun consumo»: diventa uno stato
 * dichiarato, che il footer rende come tale (regola Q). Best-effort riguarda il
 * flusso della chat, che non si blocca — non il numero, che o e' misurato o si
 * dice non misurato.
 */
export function useRipartizioneRun(
  sessionId: string | undefined,
  runId: string,
  nota: CurrentRunUsage | null | undefined,
): RipartizioneRun {
  const giaNoto = perimetroGiaNoto(nota, runId);
  const [letto, setLetto] = useState<RipartizioneRun>({ stato: "in_lettura" });

  useEffect(() => {
    if (giaNoto) return;
    if (!sessionId) {
      // Senza sessione il backend non puo' autorizzare ne' risolvere il pool del
      // progetto: la richiesta non si manda, e l'assenza si dichiara.
      setLetto({ stato: "non_disponibile", motivo: "sessione non nota" });
      return;
    }
    let vivo = true;
    setLetto({ stato: "in_lettura" });
    getSessionUsage(sessionId, runId)
      .then((usage) => {
        if (!vivo) return;
        setLetto(
          usage.currentRun
            ? { stato: "noto", perimetro: usage.currentRun }
            : {
                stato: "non_disponibile",
                motivo: "il run non risulta fra quelli di questa conversazione",
              },
        );
      })
      .catch((e: unknown) => {
        if (!vivo) return;
        setLetto({
          stato: "non_disponibile",
          motivo: e instanceof Error ? e.message : "lettura fallita",
        });
      });
    return () => {
      vivo = false;
    };
  }, [sessionId, runId, giaNoto]);

  return giaNoto && nota ? { stato: "noto", perimetro: nota } : letto;
}
