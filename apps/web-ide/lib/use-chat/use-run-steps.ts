"use client";

// Lazy-fetch degli step di un run per il nastro attivita' STORICO (ADR 0037).
//
// Al bootstrap, useChat popola metaStepsMap e traces per i run passati (via
// getSessionMetaSteps / getSessionTraces) ma NON agentStepsMap: gli step live
// arrivano via SSE e non vengono ricaricati dal DB. Percio' il nastro storico
// riceverebbe steps=[] e comporrebbe solo gli eventi derivati dai meta_step
// (executor_call/escalation/verify), SENZA i tool (che vengono dagli step).
//
// Questo hook risolve gli step: se sono gia' disponibili (agentStepsMap
// popolato) li usa senza fetch; se sono vuoti e c'e' un runId, chiama UNA sola
// volta getAgentRun(runId) e ne estrae .steps. Best-effort: se fallisce degrada
// a quel che c'e'. Punto unico riusato da MessageActivityStream e
// ActivityHistoryRow (regola L). NON tocca il turno LIVE (li' agentStepsMap e'
// gia' popolato via SSE, quindi il ramo "gia' presenti" scatta e non fa fetch).

import { useEffect, useState } from "react";
import { getAgentRun } from "../api/agent";
import type { AgentStep } from "../api/agent";
import { shouldFetchRunSteps } from "./use-run-steps-logic";

export { shouldFetchRunSteps };

/**
 * Ritorna gli step del run: quelli passati se non vuoti, altrimenti quelli
 * caricati lazy dal DB (una sola fetch per runId, best-effort).
 *
 * @param runId        id del run (necessario per il fetch)
 * @param presentSteps step gia' disponibili (agentStepsMap.get(runId) ?? [])
 * @param enabled      se false, NON esegue il fetch (usato per differirlo
 *                     all'apertura di una riga storica collassata: evita N
 *                     fetch al bootstrap). Default true.
 */
export function useResolvedRunSteps(
  runId: string | undefined,
  presentSteps: AgentStep[],
  enabled = true,
): AgentStep[] {
  const [fetched, setFetched] = useState<AgentStep[] | null>(null);
  const hasPresent = presentSteps.length > 0;

  useEffect(() => {
    // Se gli step sono gia' presenti (es. turno live via SSE, o storico gia'
    // in cache) o il fetch e' disabilitato/manca il runId, NON interroghiamo
    // il DB. Una sola fetch per (runId, enabled) grazie al guard su fetched.
    if (!shouldFetchRunSteps(hasPresent, enabled, runId)) {
      return;
    }
    let alive = true;
    getAgentRun(runId)
      .then((info) => {
        if (alive) setFetched(info.steps ?? []);
      })
      .catch(() => {
        // Best-effort: senza step il nastro mostra comunque le decisioni.
        if (alive) setFetched([]);
      });
    return () => {
      alive = false;
    };
  }, [runId, hasPresent, enabled]);

  return hasPresent ? presentSteps : fetched ?? [];
}
