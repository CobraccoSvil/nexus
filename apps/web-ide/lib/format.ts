/**
 * Fix M8 (hide IDEAI): helper per accorciare path assoluti del filesystem
 * mostrati nell'AI Workspace, evitando di esporre `/home/<user>/ideai/projects/...`
 * (path interno del prodotto Nexus) all'utente di un progetto specifico.
 *
 * Regole:
 * 1. Se il path inizia con `projectRoot` (es. `/home/.../projects/myslug`):
 *    ritorna `<slug>/<resto>` (o solo `<slug>` se path == root).
 * 2. Altrimenti taglia prefissi tipici del filesystem Nexus:
 *    `/home/<user>/ideai/projects/X`  → `~/projects/X`
 *    `/home/<user>/projects/X`        → `~/projects/X`
 *    `/home/<user>/X`                 → `~/X`
 * 3. Se nessuna regola applica, ritorna il path immutato.
 */
export function shortenAbsolutePath(absPath: string | null | undefined, projectRoot?: string | null): string {
  if (!absPath) return "";
  // Normalizza eventuali trailing slash
  const path = absPath.replace(/\/+$/, "");

  if (projectRoot) {
    const root = projectRoot.replace(/\/+$/, "");
    if (path === root) {
      const slug = root.split("/").filter(Boolean).pop() ?? "project";
      return slug;
    }
    if (path.startsWith(root + "/")) {
      const rel = path.slice(root.length + 1);
      const slug = root.split("/").filter(Boolean).pop() ?? "project";
      return `${slug}/${rel}`;
    }
  }

  // Fallback regex generici (in ordine di specificita)
  return path
    .replace(/^\/home\/[^/]+\/ideai\/projects\//, "~/projects/")
    .replace(/^\/home\/[^/]+\/projects\//, "~/projects/")
    .replace(/^\/home\/[^/]+\//, "~/");
}

/**
 * Variante "compatta" per UI con spazio limitato (es. status bar, breadcrumb).
 * Tronca a una lunghezza massima inserendo "…/" all'inizio del percorso quando troppo lungo.
 */
export function shortenAbsolutePathCompact(
  absPath: string | null | undefined,
  projectRoot?: string | null,
  maxLen = 60,
): string {
  const short = shortenAbsolutePath(absPath, projectRoot);
  if (short.length <= maxLen) return short;
  // Troncamento intelligente: mantieni primo segmento + ultimo segmento, "…" nel mezzo
  const parts = short.split("/");
  if (parts.length <= 2) return short.slice(0, maxLen - 1) + "…";
  const first = parts[0];
  const last = parts[parts.length - 1];
  return `${first}/…/${last}`;
}

// ── Formatter condivisi (regola L / ADR 0026) ───────────────────────────────
// Prima questi formatter erano definiti inline in 8+ pagine admin (formatDate
// in users/billing/project-database, formatMB/formatKB in nexus-database,
// ecc.). Ora vivono qui una volta sola, con locale di default 'it-IT' coerente
// con il resto della UI.

/**
 * Formatta una data ISO come `gg/mm/aaaa` (locale italiano di default).
 * Ritorna `'—'` per input nulli/non validi: i call site possono distinguere
 * "data assente" da "data invalida" controllando il valore di input.
 */
export function formatDate(
  iso: string | null | undefined,
  locale: string = "it-IT",
): string {
  if (!iso) return "—";
  const d = new Date(iso);
  if (Number.isNaN(d.getTime())) return "—";
  return d.toLocaleDateString(locale);
}

/**
 * Formatta una data ISO come `gg/mm/aaaa, hh:mm` (locale italiano di default).
 * Stesse regole di ``formatDate`` per gli input non validi.
 */
export function formatDateTime(
  iso: string | null | undefined,
  locale: string = "it-IT",
): string {
  if (!iso) return "—";
  const d = new Date(iso);
  if (Number.isNaN(d.getTime())) return "—";
  return d.toLocaleString(locale, { dateStyle: "short", timeStyle: "short" });
}

/**
 * Formatta un numero di byte con l'unita' piu' adatta (B/KB/MB/GB/TB).
 * Ritorna `'—'` per input nulli/negativi/invalidi.
 */
export function formatBytes(
  bytes: number | null | undefined,
  precision: number = 1,
): string {
  if (bytes == null || !Number.isFinite(bytes) || bytes < 0) return "—";
  if (bytes < 1024) return `${bytes} B`;
  const units = ["KB", "MB", "GB", "TB"];
  let value = bytes / 1024;
  let i = 0;
  while (value >= 1024 && i < units.length - 1) {
    value /= 1024;
    i += 1;
  }
  return `${value.toFixed(precision)} ${units[i]}`;
}

/**
 * Formatta un importo monetario nel locale e nella valuta indicati.
 * Default: locale ``'it-IT'``, valuta ``'USD'`` (la maggior parte dei billing
 * provider quota in dollari, ma il valore mostrato all'utente segue il locale).
 */
export function formatCurrency(
  amount: number | null | undefined,
  currency: string = "USD",
  locale: string = "it-IT",
): string {
  if (amount == null || !Number.isFinite(amount)) return "—";
  try {
    return amount.toLocaleString(locale, { style: "currency", currency });
  } catch {
    return `${amount.toFixed(2)} ${currency}`;
  }
}
