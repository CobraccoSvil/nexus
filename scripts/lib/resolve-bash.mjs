// scripts/lib/resolve-bash.mjs — localizzazione cross-platform di bash.
//
// Punto unico (regola L) condiviso dai launcher Node del repo (verify.mjs,
// smoke.mjs): su Windows `pnpm <x>` gira sotto cmd.exe, dove `bash` non e' nel
// PATH, quindi delegare a uno script .sh richiede di trovare Git Bash. Nessun
// path hardcoded come unica fonte: si prova PATH -> installazione Git ->
// percorsi standard, in quest'ordine. Ritorna il path a un bash utilizzabile,
// o null se non trovato.

import { spawnSync } from 'node:child_process';
import { existsSync } from 'node:fs';
import { dirname, join } from 'node:path';

export function resolveBash() {
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
