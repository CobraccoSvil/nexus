// Helper condivisi del client API. Punto UNICO per base URL, wrapper fetch e
// gestione errori (regola H: nessuna duplicazione della logica condivisa).
// I moduli di dominio in lib/api/* importano da qui.

import { readRenderedError, type RenderedError } from "./error-render";
import { autorizzaOblioProgetto, progettoDallUrl } from "./project-access";

export const API_BASE = typeof window !== "undefined"
  ? ""
  : (process.env.NEXT_PUBLIC_API_URL || "");
// Proxy tramite le route Next.js /api/neural/* → mcp-core :4000 /api/neural/*
// (il brain Python e' stato eliminato; evita CORS e NEXT_PUBLIC_* baked).
export const NEURAL_BASE = "/api/neural";

export function getApiBaseUrl(): string {
  return API_BASE;
}

/** Route Next.js che proxyano verso admin-service (:4010) — NON devono puntare a mcp-core (:4000). */
export function adminServiceUrl(path: string): string {
  const p = path.startsWith("/") ? path : `/${path}`;
  if (typeof window !== "undefined") {
    return `/api/admin${p}`;
  }
  // SSR: niente host relativo — proxa via Next sullo stesso origin dev (Web IDE).
  const origin =
    process.env.NEXT_INTERNAL_ORIGIN ||
    process.env.NEXT_PUBLIC_APP_ORIGIN ||
    "http://127.0.0.1:3000";
  return `${origin}/api/admin${p}`;
}

/** Errore HTTP tipizzato del client API (punto unico, regola M): lo status
 *  numerico e' il segnale strutturato su cui i call site decidono (409 = run
 *  concorrente, >=500 = ritentabile, ...). MAI ri-parsare lo status dal testo
 *  del messaggio. `message` resta identico al formato storico per il display.
 *
 *  `rendered` e' la frase gia' scritta dal backend, quando la risposta la porta
 *  (vedi lib/api/error-render.ts). Sta QUI e non nei singoli call site perche'
 *  `fetchJson` e' l'unico punto che vede ancora il payload: dopo, resta solo
 *  `message`, ed e' da li' che nascevano le classificazioni per sottostringa. */
export class ApiError extends Error {
  readonly status: number;
  readonly rendered: RenderedError | null;
  constructor(status: number, message: string, rendered: RenderedError | null = null) {
    super(message);
    this.name = "ApiError";
    this.status = status;
    this.rendered = rendered;
  }
}

/** True se l'errore indica un fallimento di trasporto o transitorio del server
 *  per cui ha senso ritentare una chiamata IDEMPOTENTE: rete giu' (fetch
 *  TypeError), timeout locale (AbortError), o HTTP 5xx. Un 4xx e' definitivo
 *  (richiesta sbagliata / conflitto di stato) e non va mai ritentato. */
export function isRetryableFetchError(e: unknown): boolean {
  if (e instanceof ApiError) return e.status >= 500;
  if (e instanceof DOMException && e.name === "AbortError") return true;
  // fetch() rigetta con TypeError su errore di rete (DNS, connessione rifiutata,
  // offline). L'abort per timeout via AbortController puo' arrivare anche come
  // stringa/valore custom (abort("timeout")): qualunque non-ApiError uscito da
  // fetchJson prima della risposta HTTP e' per costruzione un errore di trasporto.
  return e instanceof TypeError || e === "timeout";
}

/** Wrapper con retry per chiamate IDEMPOTENTI (punto unico, regola L): ritenta
 *  solo su errori di trasporto o 5xx (vedi isRetryableFetchError), con backoff.
 *  Da usare SOLO quando il server garantisce idempotenza (es. POST messaggi
 *  chat con clientMessageId, GET). Mai su POST non idempotenti. */
export async function fetchJsonWithRetry<T>(
  url: string,
  init?: RequestInit,
  timeoutMs = 30000,
  attempts = 3,
  backoffMs = 1000,
): Promise<T> {
  let lastError: unknown;
  for (let attempt = 0; attempt < attempts; attempt++) {
    if (attempt > 0) {
      await new Promise((resolve) => setTimeout(resolve, backoffMs * attempt));
    }
    try {
      return await fetchJson<T>(url, init, timeoutMs);
    } catch (e) {
      lastError = e;
      if (!isRetryableFetchError(e)) throw e;
    }
  }
  throw lastError;
}

/** Rimuove i riferimenti locali a un progetto sparito e, se la pagina puntava
 *  ancora a quello, reindirizza. Ritorna `true` solo se ha reindirizzato.
 *
 *  Senza questa pulizia l'UI continua a riprovare con l'id morto (URL
 *  `?project=`, cache dei pannelli in localStorage) e ripropone il toast di
 *  errore a ogni azione. */
function dimenticaProgetto(url: string): boolean {
  const orfano = progettoDallUrl(url);
  if (!orfano) return false;
  try {
    const daRimuovere: string[] = [];
    for (let i = 0; i < window.localStorage.length; i++) {
      const k = window.localStorage.key(i);
      // Qualunque chiave che contenga l'uuid: le entry ideai:*:{id} e le cache
      // degli altri pannelli.
      if (k?.includes(orfano)) daRimuovere.push(k);
    }
    for (const k of daRimuovere) window.localStorage.removeItem(k);
  } catch {
    // Storage non accessibile (cookie bloccati, modalita' privata): la pulizia
    // e' best-effort e non deve sostituire l'errore HTTP che il chiamante sta
    // per ricevere. Il redirect qui sotto resta comunque la cosa giusta da fare.
  }
  if (new URLSearchParams(window.location.search).get("project") !== orfano) return false;
  // Senza query: il backend selezionera' l'ultimo progetto valido.
  window.location.href = "/ide";
  return true;
}

export async function fetchJson<T>(url: string, init?: RequestInit, timeoutMs = 30000): Promise<T> {
  const controller = new AbortController();
  const timeoutId = setTimeout(() => controller.abort("timeout"), timeoutMs);
  let res: Response;
  try {
    res = await fetch(url, {
      ...init,
      credentials: "include",
      headers: { "Content-Type": "application/json", ...init?.headers },
      signal: init?.signal ?? controller.signal,
    });
  } finally {
    clearTimeout(timeoutId);
  }
  if (res.status === 401 && typeof window !== "undefined" && !window.location.pathname.startsWith("/login")) {
    window.location.href = "/login";
    throw new Error("Sessione scaduta");
  }
  // Su un 403 il client puo' dover buttare via lo stato locale del progetto. La
  // decisione passa dal CODICE canonico della resa (`user_code`), mai dalla
  // frase del corpo: vedi project-access.ts.
  if (res.status === 403 && typeof window !== "undefined") {
    const payload = await res.clone().json().catch(() => null);
    if (autorizzaOblioProgetto(readRenderedError(payload)?.code)) {
      // Ritorna true solo se ha reindirizzato: allora la richiesta in corso non
      // ha piu' un esito utile da propagare.
      if (dimenticaProgetto(String(url))) {
        throw new Error("Progetto rimosso, reindirizzamento in corso");
      }
    }
  }
  if (!res.ok) {
    let details = "";
    // La resa del backend, se c'e': l'UNICO punto in cui il payload e' ancora
    // leggibile. La stringa `message` di ApiError resta INVARIATA (formato
    // storico, letto da log e pannelli diagnostici); la frase viaggia a parte.
    let rendered: RenderedError | null = null;
    try {
      const payload = await res.json();
      rendered = readRenderedError(payload);
      const rawError =
        typeof payload?.error === "string"
          ? payload.error
          : typeof payload?.message === "string"
            ? payload.message
            : "";
      if (rawError) {
        const firstLine = rawError
          .split("\n")
          .map((line: string) => line.trim())
          .find((line: string) => line.length > 0);
        const compact = (firstLine ?? rawError).replace(/\s+/g, " ").trim();
        const reduced = compact.length > 600 ? `${compact.slice(0, 600)}...` : compact;
        details = ` - ${reduced}`;
      }
    } catch {
      // ignore body parse errors and keep generic status details
    }
    throw new ApiError(
      res.status,
      `API error ${res.status}: ${res.statusText}${details}`,
      rendered,
    );
  }
  return res.json();
}

/** Variante di fetchJson per risposte testuali (script, file di testo).
 *  Stesso timeout/AbortController e stesse credenziali; ritorna il body come stringa. */
export async function fetchText(url: string, init?: RequestInit, timeoutMs = 30000): Promise<string> {
  const controller = new AbortController();
  const timeoutId = setTimeout(() => controller.abort("timeout"), timeoutMs);
  let res: Response;
  try {
    res = await fetch(url, {
      ...init,
      credentials: "include",
      signal: init?.signal ?? controller.signal,
    });
  } finally {
    clearTimeout(timeoutId);
  }
  if (!res.ok) throw new Error(`API error ${res.status}: ${res.statusText}`);
  return res.text();
}

export async function fetchJsonNoAuth<T>(url: string, init?: RequestInit, timeoutMs = 5000): Promise<T> {
  const controller = new AbortController();
  const timeoutId = setTimeout(() => controller.abort(), timeoutMs);
  let res: Response;
  try {
    res = await fetch(url, {
      ...init,
      headers: { "Content-Type": "application/json", ...init?.headers },
      signal: init?.signal ?? controller.signal,
    });
  } finally {
    clearTimeout(timeoutId);
  }
  if (!res.ok) throw new Error(`API error ${res.status}: ${res.statusText}`);
  return res.json();
}
