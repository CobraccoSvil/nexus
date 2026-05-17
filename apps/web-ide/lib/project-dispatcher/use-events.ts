// Hook universali per consumare eventi dispatcher arbitrari.
//
// Pattern: invece di legare ogni componente a un selettore tipizzato dello
// store, qualsiasi componente puo' dichiarare "mi interessano questi eventi"
// e ricevere callback in tempo reale. Permette di reagire ad eventi nuovi
// (es. EventEnriched, MutationRecorded) senza dover estendere lo store ogni
// volta.
//
// Implementazione: lo store espone `subscribeAll(listener)` che chiama il
// listener per ogni evento applicato (in microtask, post-set). Questi hook
// sono uno strato di ergonomia sopra quella primitiva.

import { useEffect, useRef } from "react";
import { useProjectStore } from "./store";
import type { EnvelopedEvent } from "./types";

export type EventFilter = (env: EnvelopedEvent) => boolean;
export type EventHandler = (env: EnvelopedEvent) => void;

/**
 * Sottoscrive un handler a tutti gli eventi che soddisfano `filter`.
 *
 * - `filter` e `handler` vengono "freezati" via ref: NON e' necessario
 *   memoizzarli con `useCallback` lato chiamante. Solo `deps` ri-creano
 *   la sottoscrizione (passare `[]` per "sempre lo stesso filter").
 * - Cleanup automatico su unmount o cambio `deps`.
 */
export function useProjectEvents(
  filter: EventFilter,
  handler: EventHandler,
  deps: React.DependencyList = [],
): void {
  const filterRef = useRef(filter);
  const handlerRef = useRef(handler);
  filterRef.current = filter;
  handlerRef.current = handler;

  useEffect(() => {
    const subscribeAll = useProjectStore.getState().subscribeAll;
    const unsub = subscribeAll((env) => {
      try {
        if (filterRef.current(env)) {
          handlerRef.current(env);
        }
      } catch (e) {
        console.error("[useProjectEvents] handler error:", e);
      }
    });
    return unsub;
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, deps);
}

/**
 * Zucchero per il caso comune: "voglio reagire solo agli eventi con un certo
 * kind" (es. "FindingsUpdated", "ChatSessionCompacted").
 */
export function useEventOfKind<K extends EnvelopedEvent["payload"]["kind"]>(
  kind: K,
  handler: (env: EnvelopedEvent & { payload: Extract<EnvelopedEvent["payload"], { kind: K }> }) => void,
  deps: React.DependencyList = [],
): void {
  useProjectEvents(
    (env) => env.payload.kind === kind,
    (env) => handler(env as EnvelopedEvent & { payload: Extract<EnvelopedEvent["payload"], { kind: K }> }),
    deps,
  );
}
