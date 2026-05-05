"use client";

import { useMemo } from "react";
import { usePendingOperations } from "./pending-operations-context";

/**
 * Hook che fornisce versioni "tracked" delle funzioni API
 * Ogni richiesta viene automaticamente tracciata e può essere cancellata
 * se l'utente naviga via dalla pagina
 */
export function useTrackedApi() {
  const { addOperation, removeOperation } = usePendingOperations();

  return useMemo(() => ({
    /**
     * Esegui una richiesta fetch con tracciamento automatico
     * Se l'utente naviga via, la richiesta verrà cancellata
     */
    trackedFetch: async (
      url: string,
      description: string,
      init?: RequestInit
    ): Promise<Response> => {
      const { id, controller } = addOperation(description);

      try {
        const response = await fetch(url, {
          ...init,
          signal: controller.signal,
        });
        removeOperation(id);
        return response;
      } catch (error: unknown) {
        removeOperation(id);
        if (error instanceof Error && error.name === "AbortError") {
          console.log("Request aborted: " + description);
          throw new Error("Operazione annullata");
        }
        throw error;
      }
    },
  }), [addOperation, removeOperation]);
}
