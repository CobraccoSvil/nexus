// Utility colore condivise (regola L: punto unico). `withAlpha` era duplicata
// in components/chat/run-notifications.tsx e components/chat/activity-stream.tsx:
// consolidata qui, entrambi i file la importano da questo modulo.

/** Converte un colore hex (#RRGGBB) in rgba() con l'alpha indicato. Se la stringa
 *  non e' un hex a 6 cifre la ritorna invariata (degrado pulito: es. rgba/named
 *  gia' pronti). */
export function withAlpha(hex: string, alpha: number): string {
  const m = /^#?([0-9a-fA-F]{6})$/.exec(hex);
  if (!m) return hex;
  const v = m[1];
  const r = parseInt(v.slice(0, 2), 16);
  const g = parseInt(v.slice(2, 4), 16);
  const b = parseInt(v.slice(4, 6), 16);
  return `rgba(${r},${g},${b},${alpha})`;
}
