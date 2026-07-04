#!/usr/bin/env node
// scripts/verify.mjs — launcher cross-platform del gate di verifica.
//
// Il gate reale (fasi turbo/cargo/audit/quality) vive UNA volta sola in
// scripts/verify.sh (punto unico, regola L): questo wrapper NON ne duplica la
// logica, la delega. Esiste solo per rendere il gate lanciabile da qualunque
// shell. Su Windows `pnpm verify` gira sotto cmd.exe, dove `bash` non e' nel
// PATH, quindi "bash scripts/verify.sh" muore con "bash non riconosciuto";
// gli hook lefthook funzionano solo perche' usano la propria shell. Qui
// localizziamo bash (Git Bash su Windows, via il punto unico resolveBash) e
// gli deleghiamo lo script, propagando argomenti, ambiente
// (VERIFY_SKIP_RUST/TS) ed exit code.

import { spawnSync } from 'node:child_process';
import { dirname, join } from 'node:path';
import { fileURLToPath } from 'node:url';
import { resolveBash } from './lib/resolve-bash.mjs';

const ROOT = join(dirname(fileURLToPath(import.meta.url)), '..');
const SCRIPT = 'scripts/verify.sh';

const bash = resolveBash();
if (!bash) {
  console.error(
    "verify: impossibile trovare 'bash'. Su Windows installa Git for Windows " +
      '(fornisce Git Bash) o aggiungi bash.exe al PATH. Il gate vive in ' +
      'scripts/verify.sh e richiede una shell POSIX.',
  );
  process.exit(127);
}

const res = spawnSync(bash, [SCRIPT, ...process.argv.slice(2)], {
  cwd: ROOT,
  stdio: 'inherit',
  env: process.env,
});

if (res.error) {
  console.error(`verify: esecuzione di bash fallita: ${res.error.message}`);
  process.exit(1);
}
process.exit(res.status ?? 1);
