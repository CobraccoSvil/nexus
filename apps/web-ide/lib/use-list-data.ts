// use-list-data.ts — Hook condiviso per il pattern fetch+loading+error+reload
// duplicato in 10+ pagine admin (regola L / ADR 0026).
//
// Prima ogni pagina admin reinventava:
//   const [data, setData] = useState<T[]>([]);
//   const [loading, setLoading] = useState(true);
//   const [error, setError] = useState<string | null>(null);
//   const load = async () => { setLoading(true); try { ... } catch { ... } finally { ... } };
//   useEffect(() => { load(); }, []);
//
// Ora vive qui una volta sola. Non sostituisce gli hook gia' specializzati
// (es. lib/admin-hooks.ts per casi piu' complessi): copre il caso comune
// "carica una lista, mostra loading/error, esponi reload".
"use client";

import { useCallback, useEffect, useState } from "react";

export interface UseListDataResult<T> {
  data: T[];
  loading: boolean;
  error: string | null;
  reload: () => Promise<void>;
  setData: (next: T[]) => void;
}

export function useListData<T>(fetcher: () => Promise<T[]>): UseListDataResult<T> {
  const [data, setData] = useState<T[]>([]);
  const [loading, setLoading] = useState<boolean>(true);
  const [error, setError] = useState<string | null>(null);

  const reload = useCallback(async () => {
    setLoading(true);
    setError(null);
    try {
      const items = await fetcher();
      setData(items);
    } catch (e) {
      setError(e instanceof Error ? e.message : "Errore di caricamento");
    } finally {
      setLoading(false);
    }
  }, [fetcher]);

  useEffect(() => {
    void reload();
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, []);

  return { data, loading, error, reload, setData };
}
