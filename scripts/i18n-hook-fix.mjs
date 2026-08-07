#!/usr/bin/env node
// Sposta `const { t } = useI18n();` dal destructuring dei PARAMETRI al corpo
// del componente.
//
// Difetto di `i18n-hook.mjs` alla prima esecuzione: la regex che cercava
// l'inizio del corpo (`\(...\)?\s*\{\n`) ha agganciato la graffa del
// destructuring dei parametri, non quella del corpo, producendo
//
//     export function FigmaOAuthCard({
//       const { t } = useI18n();      <-- dentro i parametri
//       tc,
//
// che non e' nemmeno sintatticamente valido. Lo si ripara qui invece di
// ripristinare i file, perche' nello stesso albero ci sono le sostituzioni
// dell'estrazione, non ancora committate.

import { readFileSync, writeFileSync } from "node:fs";

const RIGA = "  const { t } = useI18n();\n";
let riparati = 0;

for (const file of process.argv.slice(2)) {
  let s = readFileSync(file, "utf8");
  // Solo il caso rotto: la riga compare SUBITO dopo una `({` di apertura
  // parametri, cioe' prima che i parametri stessi siano dichiarati.
  const rotto = /(\(\{\n)  const \{ t \} = useI18n\(\);\n/;
  if (!rotto.test(s)) continue;
  s = s.replace(rotto, "$1");

  // Il corpo comincia dopo la chiusura della firma: `}) {` oppure `) {` a
  // inizio riga. Si prende la PRIMA che segue la firma del componente.
  const m = s.match(/^(\}?\)(?::\s*[A-Za-z<>[\]|.\s]+)?\s*\{)$/m);
  if (!m) {
    console.error(`i18n-hook-fix: corpo non trovato in ${file}`);
    continue;
  }
  s = s.replace(m[0], `${m[0]}\n${RIGA.trimEnd()}`);
  writeFileSync(file, s, "utf8");
  riparati++;
}
console.log(`i18n-hook-fix: ${riparati} file riparati`);
