#!/usr/bin/env node
// Gate "ratchet" sui testi NON tradotti della web-ide: il numero puo' solo
// SCENDERE rispetto alla baseline.
//
// PERCHE' ESISTE. Al 06/08/2026 la UI e' bilingue a meta': 391 chiavi passano
// dal traduttore, 842 stringhe visibili sono letterali italiani sparsi nei
// componenti (inclusi quelli MISTI, che traducono solo in parte). Il risultato non e' «un'app italiana», e' un'app in cui due
// frasi adiacenti parlano lingue diverse — l'utente ha segnalato i banner di
// risveglio automatico in inglese in mezzo a un riepilogo in italiano.
//
// L'estrazione delle 842 richiede piu' ondate. Questo gate serve a che il
// debito non CRESCA nel frattempo: una PR che aggiunge un letterale visibile
// senza chiave lo fa salire, e il gate la ferma. La baseline si riallinea al
// ribasso dopo ogni ondata, come per jscpd e quality-scan.
//
// COSA CONTA, e perche' proprio questo. Il testo che l'utente LEGGE:
//   - attributi `title` / `placeholder` / `aria-label` (compaiono in tooltip,
//     campi vuoti e lettori di schermo);
//   - testo JSX diretto fra due tag, se inizia con una maiuscola accentata o
//     no — la maiuscola distingue una frase da un frammento tecnico.
// Conta in TUTTI i componenti, anche in quelli che gia' traducono. Saltarli
// sarebbe comodo e sbagliato due volte: `message-list.tsx` riceve `t` come
// PROP e non nomina mai `useI18n`, quindi risulterebbe «non tradotto» pur
// usando il traduttore; e i file MISTI — meta' tradotti, meta' no — sono
// esattamente il difetto che l'utente vede, due frasi adiacenti in lingue
// diverse. Un gate che li esclude misura tutto tranne il caso peggiore.
//
// LIMITE DICHIARATO: e' un'euristica testuale, non un parser JSX. Puo' contare
// una stringa che non e' testo visibile (falso positivo) o perderne una
// costruita a runtime (falso negativo). Va bene per un RATCHET, che misura una
// tendenza e non certifica una copertura: la copertura la certifica il
// compilatore, perche' `TranslationKey` e' `keyof typeof en` e una chiave
// inesistente non compila.

import { readFileSync, readdirSync, statSync, writeFileSync } from "node:fs";
import { join, relative } from "node:path";
import { fileURLToPath } from "node:url";

import { NEUTRI } from "./i18n-neutri.mjs";

const RADICE = join(fileURLToPath(new URL(".", import.meta.url)), "..");
const COMPONENTI = join(RADICE, "apps", "web-ide", "components");
const BASELINE = join(RADICE, "scripts", "i18n-baseline.json");

const ATTR = /(?:title|placeholder|aria-label)=(?:"([^"]{3,})"|\{"([^"]{3,})"\})/g;
// Il `>` di apertura non deve essere quello di una FRECCIA: `=> Promise<void>`
// in una firma di funzione dava «Promise» come testo visibile, e cosi' erano
// contate decine di firme in tutta la web-ide. Un ratchet che conta il CODICE
// non misura il testo, e la sua baseline si gonfia di cio' che nessuno leggera'
// mai a schermo — misurato: 73 residue di cui la grande maggioranza erano
// `=> Promise<`.
const JSX = /(?<!=)>\s*([A-ZÀ-Ù][^<>{}\n]{3,60}?)\s*</g;

function* tsx(dir) {
  for (const voce of readdirSync(dir)) {
    const p = join(dir, voce);
    if (statSync(p).isDirectory()) yield* tsx(p);
    else if (voce.endsWith(".tsx")) yield p;
  }
}

function conta() {
  const perArea = {};
  let totale = 0;
  for (const file of tsx(COMPONENTI)) {
    const sorgente = readFileSync(file, "utf8");
    // Le voci NEUTRE non sono debito: lo strumento di estrazione si rifiuta
    // di proporle, quindi contarle qui darebbe un residuo che nessuno puo'
    // ripagare. Punto unico della lista: `i18n-neutri.mjs`.
    const nonNeutre = (re) =>
      [...sorgente.matchAll(re)].filter((m) => {
        const testo = (m[2] ?? m[3] ?? m[1] ?? "").trim();
        return testo && !NEUTRI.has(testo);
      }).length;
    const n = nonNeutre(ATTR) + nonNeutre(JSX);
    if (!n) continue;
    const rel = relative(COMPONENTI, file).split(/[\\/]/);
    const area = rel.length > 1 ? rel[0] : "radice";
    perArea[area] = (perArea[area] ?? 0) + n;
    totale += n;
  }
  return { totale, perArea };
}

const attuale = conta();
const aggiorna = process.argv.includes("--update");

if (aggiorna) {
  writeFileSync(BASELINE, `${JSON.stringify(attuale, null, 2)}\n`, "utf8");
  console.log(`i18n-ratchet: baseline aggiornata a ${attuale.totale} stringhe non tradotte`);
  for (const [area, n] of Object.entries(attuale.perArea).sort((a, b) => b[1] - a[1])) {
    console.log(`  ${area.padEnd(20)} ${String(n).padStart(4)}`);
  }
  process.exit(0);
}

let base;
try {
  base = JSON.parse(readFileSync(BASELINE, "utf8"));
} catch {
  console.error(
    "i18n-ratchet: baseline assente. Crearla con `node scripts/i18n-ratchet.mjs --update`.",
  );
  process.exit(1);
}

const delta = attuale.totale - base.totale;
console.log(
  `i18n-ratchet: ${attuale.totale} stringhe non tradotte (baseline ${base.totale}, delta ${delta >= 0 ? "+" : ""}${delta})`,
);

if (delta > 0) {
  // Si dice DOVE e' cresciuto: un totale nudo obbliga chi legge a ricontare a
  // mano per capire cosa ha toccato.
  for (const [area, n] of Object.entries(attuale.perArea)) {
    const prima = base.perArea?.[area] ?? 0;
    if (n > prima) console.error(`  ${area}: ${prima} -> ${n}`);
  }
  console.error(
    "i18n-ratchet: testo non tradotto AUMENTATO. Usa il traduttore (useI18n + chiave nei tre dizionari) " +
      "invece di scrivere il testo nel componente; se l'aumento e' giustificato, riallinea la baseline e dichiaralo nel commit.",
  );
  process.exit(1);
}

if (delta < 0) {
  console.log("i18n-ratchet: migliorato, riallinea con `--update` e dichiaralo nel commit.");
}
process.exit(0);
