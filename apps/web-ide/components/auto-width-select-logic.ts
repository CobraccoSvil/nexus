export interface AutoWidthSelectOption {
  value: string;
  label: string;
  disabled?: boolean;
}

export interface AutoWidthSelectGroup {
  label: string;
  options: readonly AutoWidthSelectOption[];
}

export type AutoWidthSelectItem = AutoWidthSelectOption | AutoWidthSelectGroup;

export function isGroup(item: AutoWidthSelectItem): item is AutoWidthSelectGroup {
  return "options" in item;
}

export function flatten(items: readonly AutoWidthSelectItem[]): AutoWidthSelectOption[] {
  return items.flatMap((item) => (isGroup(item) ? [...item.options] : [item]));
}

/**
 * Etichetta che il select mostra davvero, e quindi l'unica che il fantasma deve
 * misurare.
 *
 * Il fallback non e' una comodita': quando il valore non corrisponde a nessuna
 * opzione, un <select> nativo mostra comunque la PRIMA. Misurare il valore
 * orfano darebbe una pillola larga quanto una stringa che nessuno vede.
 */
export function etichettaVisibile(
  items: readonly AutoWidthSelectItem[],
  value: string | undefined,
): string {
  const piatte = flatten(items);
  const selezionata = piatte.find((option) => option.value === value) ?? piatte[0];
  return selezionata ? selezionata.label : "";
}
