// Helper condivisi del client API. Punto UNICO per base URL, wrapper fetch e
// gestione errori (regola H: nessuna duplicazione della logica condivisa).
// I moduli di dominio in lib/api/* importano da qui.

export const API_BASE = typeof window !== "undefined"
  ? ""
  : (process.env.NEXT_PUBLIC_API_URL || "");
// Proxy tramite Next.js /api/neural/* → brain:8001 (evita CORS e NEXT_PUBLIC_* baked)
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
  // Bug-fix: quando il backend ritorna 403 "Progetto non accessibile" significa
  // che il progetto e' stato cancellato (server-side) ma il client ha ancora
  // riferimenti stale (URL ?project=, localStorage activeTab, ecc.). Senza
  // questo cleanup l'UI continua a riprovare la PUT e mostra il toast
  // "Operazione progetto (PUT) fallita: Progetto non accessibile" ad ogni
  // refresh / azione.
  if (res.status === 403 && typeof window !== "undefined") {
    try {
      const cloned = res.clone();
      const payload = await cloned.json().catch(() => null);
      const errText =
        typeof payload?.error === "string"
          ? payload.error
          : typeof payload?.message === "string"
            ? payload.message
            : "";
      if (errText.includes("Progetto non accessibile")) {
        // Estrae l'UUID del progetto dall'URL chiamato (pattern /api/projects/{uuid}/...)
        const m = String(url).match(/\/api\/projects\/([0-9a-f-]{36})/i);
        if (m) {
          const orphanId = m[1];
          const keysToDrop: string[] = [];
          for (let i = 0; i < window.localStorage.length; i++) {
            const k = window.localStorage.key(i);
            if (!k) continue;
            // Rimuove tutte le entry ideai:*:{orphanId} e qualsiasi chiave che
            // contenga l'UUID orfano (cache di altri pannelli)
            if (k.includes(orphanId)) keysToDrop.push(k);
          }
          for (const k of keysToDrop) window.localStorage.removeItem(k);
          // Se l'URL della pagina punta ancora a quel progetto orfano,
          // forza il redirect a /ide senza query (il backend selezionera'
          // automaticamente l'ultimo progetto valido).
          const currentParam = new URLSearchParams(window.location.search).get("project");
          if (currentParam === orphanId) {
            window.location.href = "/ide";
            throw new Error("Progetto rimosso, reindirizzamento in corso");
          }
        }
      }
    } catch (cleanupErr) {
      // se il cleanup fallisce non bloccare il flow di errore originale
      if (cleanupErr instanceof Error && cleanupErr.message.includes("reindirizzamento")) {
        throw cleanupErr;
      }
    }
  }
  if (!res.ok) {
    let details = "";
    try {
      const payload = await res.json();
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
    throw new Error(`API error ${res.status}: ${res.statusText}${details}`);
  }
  return res.json();
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
