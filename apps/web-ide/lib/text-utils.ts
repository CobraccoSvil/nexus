/**
 * Utility per gestire testi troncati con tooltip.
 * Nella UI, quando un elemento ha `overflow: hidden` con `text-overflow: ellipsis`,
 * aggiungi `title={getTruncatePropsFull(text).title}` per mostrare il testo completo in tooltip.
 */

/**
 * Restituisce title attribute se il testo supera maxLength.
 * Utile per testi che potrebbero essere lunghi ma non sempre.
 *
 * @param text - Testo da verificare
 * @param maxLength - Lunghezza massima prima di aggiungere title (default: 50)
 * @returns Oggetto con title attribute opzionale
 *
 * @example
 * const { title } = getTruncateProps(fileName, 30);
 * <span title={title}>{fileName}</span>
 */
export function getTruncateProps(text: string | undefined | null, maxLength = 50): { title?: string } {
  if (!text) return {};
  const trimmed = text.trim();
  return trimmed.length > maxLength ? { title: trimmed } : {};
}

/**
 * Restituisce sempre title attribute con il testo completo.
 * Utile per path, hash commit, comandi — sempre interessante vederli in tooltip.
 *
 * @param text - Testo da mostrare in tooltip
 * @returns Oggetto con title attribute
 *
 * @example
 * const { title } = getTruncatePropsFull(filePath);
 * <span title={title}>{filePath}</span>
 */
export function getTruncatePropsFull(text: string | undefined | null): { title?: string } {
  return text ? { title: text.trim() } : {};
}

/**
 * Variante compatta: ritorna direttamente il valore di title.
 *
 * @param text - Testo da mostrare in tooltip
 * @returns Stringa per title attribute o undefined
 *
 * @example
 * <span title={getTruncateTitle(filePath)}>{filePath}</span>
 */
export function getTruncateTitle(text: string | undefined | null): string | undefined {
  return text ? text.trim() : undefined;
}
