// Formattazione dei valori del pannello Monitor (punto unico, regola L).
//
// I monitor hanno `value` scalare (numero/stringa, es. "76" run, un modello) MA
// alcune metriche seed di servizio emettono un OGGETTO di risorse
// (`{cpu_pct, rss_bytes, io_read_bytes, io_write_bytes}`, da process_util.rs).
// Il vecchio renderer faceva `JSON.stringify` -> la card mostrava il JSON grezzo
// (`{"cpu_pct":0,"rss_bytes":6729728,...}`). Qui l'oggetto viene umanizzato.

/** Byte in forma umana con separatore decimale italiano: 6729728 -> "6,4 MB". */
export function humanBytes(n: number): string {
  if (!Number.isFinite(n) || n < 0) return String(n);
  const units = ["B", "KB", "MB", "GB", "TB"];
  let value = n;
  let u = 0;
  while (value >= 1024 && u < units.length - 1) {
    value /= 1024;
    u += 1;
  }
  // Interi come tali (128 B), il resto con un decimale (6,4 MB).
  const s = u === 0 ? String(Math.round(value)) : value.toFixed(1).replace(".", ",");
  return `${s} ${units[u]}`;
}

/** CPU% arrotondata (0 o 1 decimale), separatore italiano: 12.34 -> "12,3". */
function formatPct(n: number): string {
  const r = Math.round(n * 10) / 10;
  return (Number.isInteger(r) ? String(r) : r.toFixed(1)).replace(".", ",");
}

/** True se l'oggetto ha la forma delle metriche di risorsa di un servizio. */
function isResourceMetrics(o: Record<string, unknown>): boolean {
  return typeof o.cpu_pct === "number" && typeof o.rss_bytes === "number";
}

/** Valore di un monitor in forma leggibile. Mai JSON grezzo. */
export function formatMonitorValue(v: unknown): string {
  if (v === null || v === undefined) return "—";
  if (typeof v === "object") {
    const o = v as Record<string, unknown>;
    if (isResourceMetrics(o)) {
      // Le due KPI che contano nella card sintetica: CPU e memoria.
      const parts = [`CPU ${formatPct(o.cpu_pct as number)}%`, `RAM ${humanBytes(o.rss_bytes as number)}`];
      const io = (o.io_read_bytes as number) + (o.io_write_bytes as number);
      if (Number.isFinite(io) && io > 0) {
        parts.push(`I/O ${humanBytes(io)}`);
      }
      return parts.join(" · ");
    }
    // Oggetto generico: "chiave: valore" leggibile, non JSON con virgolette.
    const entries = Object.entries(o);
    if (entries.length === 0) return "—";
    return entries.map(([k, val]) => `${k}: ${String(val)}`).join(" · ");
  }
  return String(v);
}
