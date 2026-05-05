"use client";

import { useCallback } from "react";
import { usePendingOperations } from "./pending-operations-context";

export function useApiWithAbort() {
  const { addOperation, removeOperation } = usePendingOperations();

  const fetchWithTracking = useCallback(
    async (
      url: string,
      description: string,
      options?: RequestInit
    ): Promise<Response> => {
      const { id, controller } = addOperation(description);

      try {
        const response = await fetch(url, {
          ...options,
          signal: controller.signal,
        });
        return response;
      } finally {
        removeOperation(id);
      }
    },
    [addOperation, removeOperation]
  );

  return { fetchWithTracking };
}
