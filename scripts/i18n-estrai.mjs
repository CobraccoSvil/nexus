#!/usr/bin/env node
// Strumento di lavoro per l'estrazione i18n della web-ide, un'AREA per volta.
//
// Non traduce: prepara e applica. La traduzione la scrive una persona (o un
// modello) nel file di lavoro, perche' e' l'unica parte che richiede giudizio —
// "Annulla" e' "Cancel" in un dialogo e "Undo" in una barra strumenti, e nessuna
// tabella lo sa.
//
//   node scripts/i18n-estrai.mjs <area> --proponi   -> scrive il file di lavoro
//   node scripts/i18n-estrai.mjs <area> --applica   -> dizionari + sostituzioni
//
// PERCHE' UNO STRUMENTO E NON 109 MODIFICHE A MANO. Le prime cinque estrazioni
// sono state fatte a mano e hanno prodotto un difetto che compilava: l'inglese
// finito nel dizionario italiano, per un indice di colonna sbagliato. Nessun
// test lo avrebbe visto. Qui la lingua e' una CHIAVE nel file di lavoro, non
// una posizione, e le colonne non sono scambiabili per costruzione.

import { readFileSync, writeFileSync, readdirSync, statSync, existsSync } from "node:fs";
import { join, relative } from "node:path";
import { fileURLToPath } from "node:url";

const RADICE = join(fileURLToPath(new URL(".", import.meta.url)), "..");
const WEB = join(RADICE, "apps", "web-ide");
const COMPONENTI = join(WEB, "components");
const LAVORO = join(RADICE, "scripts", "i18n-lavoro");

const ATTR = /(title|placeholder|aria-label)=(?:"([^"]{3,})"|\{"([^"]{3,})"\})/g;
const JSX = />(\s*)([A-ZÀ-Ù][^<>{}\n]{2,60}?)(\s*)</g;

import { NEUTRI } from "./i18n-neutri.mjs";

function* tsx(dir) {
  for (const v of readdirSync(dir)) {
    const p = join(dir, v);
    if (statSync(p).isDirectory()) yield* tsx(p);
    else if (v.endsWith(".tsx")) yield p;
  }
}

function fileArea(area) {
  const base = area === "radice" ? COMPONENTI : join(COMPONENTI, area);
  const out = [];
  for (const f of tsx(base)) {
    if (area === "radice" && relative(COMPONENTI, f).includes("\\")) continue;
    if (area === "radice" && relative(COMPONENTI, f).includes("/")) continue;
    out.push(f);
  }
  return out;
}

/** Chiave stabile e leggibile dal testo: `chat.msg.nessunoStepRegistrato`. */
function chiaveDa(area, testo) {
  const parole = testo
    .normalize("NFD")
    .replace(/[̀-ͯ]/g, "")
    .replace(/[^a-zA-Z0-9 ]/g, " ")
    .trim()
    .split(/\s+/)
    .slice(0, 4);
  const camel = parole
    .map((p, i) => (i === 0 ? p.toLowerCase() : p[0].toUpperCase() + p.slice(1).toLowerCase()))
    .join("");
  return `${area}.${camel || "testo"}`;
}

function estrai(area) {
  const trovate = new Map(); // testo -> {chiave, occorrenze}
  for (const file of fileArea(area)) {
    const s = readFileSync(file, "utf8");
    for (const m of s.matchAll(ATTR)) {
      const testo = (m[2] ?? m[3]).trim();
      if (NEUTRI.has(testo) || !/[a-zà-ù]/.test(testo)) continue;
      if (!trovate.has(testo)) trovate.set(testo, { chiave: chiaveDa(area, testo), n: 0 });
      trovate.get(testo).n++;
    }
    for (const m of s.matchAll(JSX)) {
      const testo = m[2].trim();
      if (NEUTRI.has(testo) || !/[a-zà-ù]/.test(testo)) continue;
      if (!trovate.has(testo)) trovate.set(testo, { chiave: chiaveDa(area, testo), n: 0 });
      trovate.get(testo).n++;
    }
  }
  return trovate;
}

const area = process.argv[2];
const modo = process.argv.includes("--applica") ? "applica" : "proponi";
if (!area) {
  console.error("uso: i18n-estrai.mjs <area> [--proponi|--applica]");
  process.exit(1);
}
const fileLavoro = join(LAVORO, `${area}.json`);

if (modo === "proponi") {
  const trovate = estrai(area);
  // Chiavi duplicate: due testi diversi che generano lo stesso nome. Si
  // distinguono col suffisso, invece di sovrascriversi in silenzio.
  const usate = new Map();
  const voci = [];
  for (const [testo, info] of trovate) {
    let k = info.chiave;
    if (usate.has(k)) {
      const n = usate.get(k) + 1;
      usate.set(k, n);
      k = `${k}${n}`;
    } else usate.set(k, 1);
    voci.push({ chiave: k, it: testo, en: "", es: "", occorrenze: info.n });
  }
  voci.sort((a, b) => b.occorrenze - a.occorrenze);
  writeFileSync(fileLavoro, `${JSON.stringify(voci, null, 2)}\n`, "utf8");
  console.log(`${area}: ${voci.length} testi distinti -> ${relative(RADICE, fileLavoro)}`);
  console.log("Compila i campi `en` e `es`, poi rilancia con --applica.");
  process.exit(0);
}

// --applica
if (!existsSync(fileLavoro)) {
  console.error(`i18n-estrai: manca ${relative(RADICE, fileLavoro)} (esegui prima --proponi)`);
  process.exit(1);
}
const voci = JSON.parse(readFileSync(fileLavoro, "utf8"));
const senzaTraduzione = voci.filter((v) => !v.en || !v.es);
if (senzaTraduzione.length) {
  console.error(`i18n-estrai: ${senzaTraduzione.length} voci senza en/es. Prima: ${senzaTraduzione[0].it}`);
  process.exit(1);
}
// Colonne non scambiabili: la lingua e' una chiave, e un testo identico nelle
// tre lingue va dichiarato tale (succede: "AI", "Provider:").
for (const v of voci) {
  if (v.it === v.en && v.it === v.es && !v.neutro) {
    console.error(`i18n-estrai: "${v.it}" identico nelle tre lingue. Se e' voluto aggiungi "neutro": true.`);
    process.exit(1);
  }
}

for (const lang of ["it", "en", "es"]) {
  const p = join(WEB, "lib", "i18n", "dictionaries", `${lang}.ts`);
  let s = readFileSync(p, "utf8");
  // Il dizionario chiude con `} as const;` (non `};`): cercare la graffa nuda
  // inseriva le chiavi DOPO la chiusura dell'oggetto, e il file non compilava.
  // Si ancora alla chiusura REALE, e si dichiara se non la si trova invece di
  // scrivere in un punto qualsiasi.
  // I tre file NON chiudono allo stesso modo: `en` con `} as const;`, `it` ed
  // `es` con `};`. Cercare una sola forma inseriva le chiavi DOPO la chiusura,
  // e il file non compilava. Si prova la forma piu' specifica per prima.
  let ultimo = s.lastIndexOf("} as const;");
  if (ultimo < 0) ultimo = s.lastIndexOf("};");
  if (ultimo < 0) {
    console.error(`i18n-estrai: chiusura dell'oggetto non trovata in ${lang}.ts`);
    process.exit(1);
  }
  const blocco = voci
    .filter((v) => !s.includes(`"${v.chiave}":`))
    .map((v) => `    "${v.chiave}": ${JSON.stringify(v[lang])},\n`)
    .join("");
  s = `${s.slice(0, ultimo)}${blocco}${s.slice(ultimo)}`;
  writeFileSync(p, s, "utf8");
}

let sostituzioni = 0;
for (const file of fileArea(area)) {
  let s = readFileSync(file, "utf8");
  const prima = s;
  for (const v of voci) {
    const esc = v.it.replace(/[.*+?^${}()|[\]\\]/g, "\\$&");
    s = s.replace(new RegExp(`(title|placeholder|aria-label)="${esc}"`, "g"), `$1={t("${v.chiave}")}`);
    s = s.replace(new RegExp(`>(\\s*)${esc}(\\s*)<`, "g"), `>$1{t("${v.chiave}")}$2<`);
  }
  if (s !== prima) {
    writeFileSync(file, s, "utf8");
    sostituzioni++;
  }
}
console.log(`${area}: ${voci.length} chiavi nei 3 dizionari, ${sostituzioni} file modificati.`);
console.log("Ora `npx tsc --noEmit`: gli errori indicano dove manca `t` (hook o prop).");
