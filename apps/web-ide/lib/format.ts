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
