/**
 * CobraccoMark — wordmark del brand "coBRAcco" con BRA in grassetto.
 *
 * Il brand Cobracco va SEMPRE scritto come `co<strong>BRA</strong>cco`
 * nei testi visualizzati (footer, copyright, by-line). Questo componente
 * garantisce la resa coerente in tutto il sito statico.
 *
 * NB: usare la forma plain "Cobracco" SOLO in attributi tecnici (href,
 * aria-label, alt), MAI in testo visibile.
 */
export function CobraccoMark({ className }: { className?: string }) {
  return (
    <span className={className}>
      co<strong>BRA</strong>cco
    </span>
  );
}
