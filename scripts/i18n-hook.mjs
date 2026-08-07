#!/usr/bin/env node
// Aggiunge `useI18n()` ai componenti che usano `t(` senza averlo in scope.
//
// Complemento di `i18n-estrai.mjs`: quella sostituisce i letterali con
// `t("chiave")`, questo procura il traduttore. Sono due passi separati perche'
// il secondo si guida col COMPILATORE — si esegue, si ricompila, e cio' che
// resta rosso e' il caso che richiede giudizio (una funzione che non e' un
// componente React, e che quindi deve ricevere `t` come prop).
//
// COSA FA, e i limiti che dichiara:
//   - inserisce l'import se manca;
//   - inserisce `const { t } = useI18n();` come prima riga del corpo del
//     componente ESPORTATO, se il file ne ha uno solo;
//   - NON tocca i file con piu' componenti esportati o con funzioni interne che
//     usano `t`: li' la scelta fra hook e prop non e' meccanica, e indovinarla
//     produrrebbe un hook chiamato fuori da un componente — cioe' un errore a
//     runtime che il compilatore non vede.
//
// Uso: node scripts/i18n-hook.mjs <file...>

import { readFileSync, writeFileSync } from "node:fs";

const IMPORT = 'import { useI18n } from "';

function profondita(file) {
  // components/settings/x.tsx -> ../../lib/i18n ; components/settings/a/b.tsx -> ../../../lib/i18n
  const parti = file.replace(/\\/g, "/").split("/");
  const dentroComponents = parti.slice(parti.indexOf("components") + 1).length - 1;
  return "../".repeat(dentroComponents + 1) + "lib/i18n";
}

let toccati = 0;
let saltati = [];

for (const file of process.argv.slice(2)) {
  let s = readFileSync(file, "utf8");
  if (s.includes("useI18n")) continue;

  const esportati = [...s.matchAll(/^export (?:default )?function ([A-Za-z0-9_]+)/gm)];
  if (esportati.length !== 1) {
    saltati.push(`${file} (${esportati.length} componenti esportati)`);
    continue;
  }

  // Import: dopo l'ultimo import esistente, per non spezzare il blocco.
  const ultimoImport = s.lastIndexOf("\nimport ");
  const fineImport = s.indexOf("\n", s.indexOf(";", ultimoImport)) + 1;
  s = `${s.slice(0, fineImport)}${IMPORT}${profondita(file)}";\n${s.slice(fineImport)}`;

  // Hook: prima riga del corpo del componente esportato.
  const nome = esportati[0][1];
  const re = new RegExp(`(export (?:default )?function ${nome}\\([^]*?\\)?\\s*\\{\\n)`);
  const m = s.match(re);
  if (!m) {
    saltati.push(`${file} (corpo di ${nome} non riconosciuto)`);
    continue;
  }
  s = s.replace(re, `$1  const { t } = useI18n();\n`);
  writeFileSync(file, s, "utf8");
  toccati++;
}

console.log(`i18n-hook: ${toccati} file con lo hook aggiunto`);
if (saltati.length) {
  console.log(`  da fare a mano (${saltati.length}):`);
  for (const s of saltati) console.log(`    ${s}`);
}
