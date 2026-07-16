#!/usr/bin/env node
// scripts/install-hooks.mjs — installa gli hook git; e' il 'prepare' di pnpm install.
//
// Perche' esiste (fail-closed, regola M):
//   Il prepare era `lefthook install || true`. Il `|| true` copriva un caso legittimo
//   reale — installare le dipendenze dove NON c'e' un repo git (tarball, container,
//   checkout parziale): li' gli hook non sono installabili e non e' un errore. Ma
//   sopprimeva QUALUNQUE errore, quindi copriva anche il caso opposto: se
//   l'installazione degli hook fallisce davvero, pnpm install non se ne accorge e
//   resti senza gate, in silenzio. E' la stessa classe di falso verde dell'hook che
//   non trovava lefthook (vedi .lefthookrc): assenza dello strumento di verifica
//   trattata come "verifica a posto".
//
//   In piu' `|| true` non manteneva la promessa su Windows, l'ambiente canonico del
//   progetto: gli script npm girano sotto cmd.exe (vedi scripts/verify.mjs), dove
//   `true` non esiste. Verificato su cmd.exe: 'comando-fallito || true' esce 1, non 0
//   — l'errore veniva propagato, ma travestito da '"true" non e' riconosciuto come
//   comando interno o esterno', una diagnosi che non nomina mai gli hook. Quindi:
//   falso verde su sh/CI, diagnosi fuorviante su Windows.
//
// Qui i due casi sono distinti da un segnale strutturato, non da un catch-all:
//   - nessun repo git (git rev-parse --git-dir != 0) -> skip dichiarato, exit 0;
//   - lefthook install fallito                       -> exit != 0, pnpm install si ferma.
//
// NB: `pnpm install --ignore-scripts` non esegue affatto il prepare, quindi non
// installa gli hook. E' una scelta esplicita di chi lancia il comando, non un
// fallimento che questo script possa intercettare.

import { spawnSync } from 'node:child_process';

// Segnale strutturato #1: esiste un repo git in cui installare gli hook?
const gitProbe = spawnSync('git', ['rev-parse', '--git-dir'], { stdio: 'ignore' });

if (gitProbe.error || gitProbe.status !== 0) {
  console.log(
    'install-hooks: nessun repository git qui, hook git non installati. ' +
      'Caso previsto (tarball/container/checkout parziale): non e\' un errore.',
  );
  process.exit(0);
}

// shell: true perche' su Windows il binario e' esposto come lefthook.CMD in
// node_modules/.bin, e da Node 20 uno .cmd non e' eseguibile da spawn senza shell.
// Comando in stringa unica e non (cmd, args[]): con shell: true Node concatena gli
// argomenti senza escaparli ed emette DeprecationWarning DEP0190 a ogni pnpm install.
// La riga e' costante, nessun input esterno vi finisce dentro.
const install = spawnSync('lefthook install', { stdio: 'inherit', shell: true });

// Segnale strutturato #2: l'exit code di lefthook, non il testo che ha stampato.
if (install.error || install.status !== 0) {
  console.error('');
  console.error('install-hooks: lefthook install FALLITO -> hook git NON installati.');
  console.error('  Senza hook nessun gate pre-commit gira e i commit entrano non');
  console.error('  verificati, quindi questo errore blocca l\'install invece di essere');
  console.error('  ignorato: un gate non installato non e\' un gate superato.');
  console.error('  Verificare lefthook.yml e che la dipendenza lefthook sia installata.');
  process.exit(install.status ?? 1);
}
