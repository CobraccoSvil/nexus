// admin-hooks.ts — Hook condivisi per le pagine admin (Fase G del piano).
// Estrae il pattern ricorrente "carica lista da endpoint + stato loading/error"
// presente in quasi tutte le pagine di amministrazione.
"use client";

import { useCallback, useEffect, useState } from "react";

const API = process.env.NEXT_PUBLIC_API_URL || "";

/**
 * useAdminList — carica una lista da un endpoint admin (GET, credentials
 * include) gestendo loading/error e fornendo un reload manuale.
 */
export function useAdminList<T>(endpoint: string) {
  const [items, setItems] = useState<T[]>([]);
  const [loading, setLoading] = useState(true);
  const [error, setError] = useState("");

  const reload = useCallback(async () => {
    setLoading(true);
    setError("");
    try {
      const r = await fetch(`${API}${endpoint}`, { credentials: "include" });
      if (r.ok) {
        setItems(await r.json());
      } else {
        setError(`Errore ${r.status}`);
      }
    } catch (e) {
      setError(String(e));
    } finally {
      setLoading(false);
    }
  }, [endpoint]);

  useEffect(() => {
    reload();
  }, [reload]);

  return { items, loading, error, reload, setError, setItems };
}
