"use client";

// Lettura del feature flag `chat.activity_stream_enabled` (ADR 0037 sez. 5).
//
// Fonte di verita': la tabella `settings` nel DB (regola G: niente env var,
// niente default hardcoded diverso dal DB). Il valore si legge via la route
// proxy Next `GET /api/ui-flags`, che inoltra a mcp-core `/api/ui-flags`
// (require_auth, NON admin): i flag di RENDERING della chat devono essere
// leggibili da QUALUNQUE utente autenticato, non solo dagli admin, altrimenti
// la feature resterebbe inerte per gli utenti normali (feature morta
// silenziosa). Cache client 60s (come la cache lato Rust dei settings) per non
// interrogare il backend a ogni render.
//
// DEFAULT OFF: in assenza del setting, di autorizzazione o su errore, il flag
// e' `false` -> il rendering resta IDENTICO a oggi (OFF = bit-identico). I
// componenti activity-stream si attivano solo quando il DB porta
// `chat.activity_stream_enabled = 'true'`.

import { useEffect, useState } from "react";

const FLAG_KEY = "chat.activity_stream_enabled";
const TTL_MS = 60_000;

// Cache modulo-locale (una sola richiesta ogni 60s per tutta l'app).
let _cache: { loadedAt: number; enabled: boolean } | null = null;
let _inflight: Promise<boolean> | null = null;

/** true se la stringa rappresenta un booleano-vero ("true"/"1"/"on"/"yes"). */
function parseBool(value: string | undefined): boolean {
  if (!value) return false;
  const v = value.trim().toLowerCase();
  return v === "true" || v === "1" || v === "on" || v === "yes";
}

async function fetchFlag(): Promise<boolean> {
  const now = Date.now();
  if (_cache && now - _cache.loadedAt < TTL_MS) return _cache.enabled;
  if (_inflight) return _inflight;

  _inflight = (async () => {
    try {
      const res = await fetch(`/api/ui-flags`, { credentials: "include" });
      if (!res.ok) {
        // Non autorizzato / backend giu': default OFF.
        _cache = { loadedAt: now, enabled: false };
        return false;
      }
      const data = (await res.json()) as { flags?: Record<string, string> };
      const enabled = parseBool(data.flags?.[FLAG_KEY]);
      _cache = { loadedAt: now, enabled };
      return enabled;
    } catch {
      _cache = { loadedAt: now, enabled: false };
      return false;
    } finally {
      _inflight = null;
    }
  })();
  return _inflight;
}

/**
 * Hook: ritorna se il nastro attivita' e' abilitato. Parte da `false` (OFF) e
 * si aggiorna quando la fetch risolve. Cache 60s condivisa tra tutte le
 * istanze: montare l'hook in piu' punti NON moltiplica le richieste.
 */
export function useActivityStreamEnabled(): boolean {
  const [enabled, setEnabled] = useState<boolean>(_cache?.enabled ?? false);
  useEffect(() => {
    let alive = true;
    void fetchFlag().then((v) => {
      if (alive) setEnabled(v);
    });
    return () => {
      alive = false;
    };
  }, []);
  return enabled;
}

/** Invalida la cache del flag (utile dopo un cambio in /admin/settings). */
export function invalidateActivityStreamFlag(): void {
  _cache = null;
}
