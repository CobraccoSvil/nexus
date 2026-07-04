#!/usr/bin/env node
// scripts/verify.mjs — launcher cross-platform del gate di verifica.
//
// Il gate reale (fasi turbo/cargo/audit/quality) vive UNA volta sola in
// scripts/verify.sh (punto unico, regola L): questo wrapper NON ne duplica la
// logica, la delega. Esiste solo per rendere il gate lanciabile da qualunque
// shell. Su Windows `pnpm verify` gira sotto cmd.exe, dove `bash` non e' nel
// PATH, quindi "bash scripts/verify.sh" muore con "bash non riconosciuto";
// gli hook lefthook funzionano solo perche' usano la propria shell. Qui
// localizziamo bash (Git Bash su Windows) e gli deleghiamo lo script,
// propagando argomenti, ambiente (VERIFY_SKIP_RUST/TS) ed exit code.

import { spawnSync } from 'node:child_process';
import { existsSync } from 'node:fs';
import { dirname, join } from 'node:path';
import { fileURLToPath } from 'node:url';

const ROOT = join(dirname(fileURLToPath(import.meta.url)), '..');
const SCRIPT = 'scripts/verify.sh';

// Ritorna il path a un bash utilizzabile, o null se non trovato.
// Nessun path hardcoded come unica fonte: si prova PATH -> installazione Git
// -> percorsi standard, in quest'ordine.
function resolveBash() {
  // Unix/macOS: bash e' nel PATH.
  if (process.platform !== 'win32') return 'bash';

  // 1. bash gia' raggiungibile (shell Git Bash, o bash nel PATH di sistema).
  const onPath = spawnSync('where', ['bash.exe'], { encoding: 'utf8' });
  if (onPath.status === 0) {
    const first = onPath.stdout.split(/\r?\n/).find((l) => l.trim());
    if (first && existsSync(first.trim())) return first.trim();
  }

  // 2. Derivato dall'installazione di Git: where git -> <Git>\cmd\git.exe
  //    (o <Git>\bin\git.exe) -> risali alla root Git -> bin\bash.exe.
  const whereGit = spawnSync('where', ['git.exe'], { encoding: 'utf8' });
  if (whereGit.status === 0) {
    for (const line of whereGit.stdout.split(/\r?\n/)) {
      const gitExe = line.trim();
      if (!gitExe) continue;
      const gitRoot = dirname(dirname(gitExe));
      for (const cand of [
        join(gitRoot, 'bin', 'bash.exe'),
        join(gitRoot, 'usr', 'bin', 'bash.exe'),
      ]) {
        if (existsSync(cand)) return cand;
      }
    }
  }

  // 3. Percorsi di installazione standard di Git for Windows.
  const candidates = [
    join(process.env.ProgramFiles ?? 'C:\\Program Files', 'Git', 'bin', 'bash.exe'),
    join(
      process.env['ProgramFiles(x86)'] ?? 'C:\\Program Files (x86)',
      'Git',
      'bin',
      'bash.exe',
    ),
    join(process.env.LOCALAPPDATA ?? '', 'Programs', 'Git', 'bin', 'bash.exe'),
  ];
  for (const cand of candidates) {
    if (cand && existsSync(cand)) return cand;
  }

  return null;
}

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
