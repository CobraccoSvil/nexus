#!/usr/bin/env node
// scripts/smoke.mjs — launcher cross-platform dello smoke test dei servizi.
//
// Gemello di verify.mjs (regola L: condividono il punto unico resolveBash). Lo
// smoke reale (avvio servizi + check porte) vive UNA volta sola in
// scripts/smoke-services.sh: questo wrapper NON ne duplica la logica, la delega.
// Esiste solo per rendere lo smoke lanciabile da qualunque shell: su Windows
// `pnpm smoke` gira sotto cmd.exe, dove `bash` non e' nel PATH, quindi
// "bash scripts/smoke-services.sh" muore con "bash non riconosciuto". Qui
// localizziamo bash (Git Bash su Windows) e gli deleghiamo lo script,
// propagando argomenti, ambiente (porte via env) ed exit code.

import { spawnSync } from 'node:child_process';
import { dirname, join } from 'node:path';
import { fileURLToPath } from 'node:url';
import { resolveBash } from './lib/resolve-bash.mjs';

const ROOT = join(dirname(fileURLToPath(import.meta.url)), '..');
const SCRIPT = 'scripts/smoke-services.sh';

const bash = resolveBash();
if (!bash) {
  console.error(
    "smoke: impossibile trovare 'bash'. Su Windows installa Git for Windows " +
      '(fornisce Git Bash) o aggiungi bash.exe al PATH. Lo smoke vive in ' +
      'scripts/smoke-services.sh e richiede una shell POSIX.',
  );
  process.exit(127);
}

const res = spawnSync(bash, [SCRIPT, ...process.argv.slice(2)], {
  cwd: ROOT,
  stdio: 'inherit',
  env: process.env,
});

if (res.error) {
  console.error(`smoke: esecuzione di bash fallita: ${res.error.message}`);
  process.exit(1);
}
process.exit(res.status ?? 1);
