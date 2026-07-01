// Batch refactor qualita Rust su Windows nativo (D:\IDEAI).
// IMPORTANTE: lanciare SOLO da una sessione Claude Code con working directory
// su D:\IDEAI (path Windows reale). Da una sessione con cwd UNC \\wsl.localhost
// i subagenti NON eseguono shell (vedi memoria reference_subagenti_cwd_unc_windows).
//
// La lista FILES va INCORPORATA qui sotto (NON passata via args: args non viene
// iniettato quando si passa lo script inline). Per rigenerarla:
//   .\target\debug\xtask.exe quality-scan --export %TEMP%\wl.json
// poi escludere i file gia committati su quality/win-refactor e incollare i path.

export const meta = {
  name: 'win-quality-batch',
  description: 'Batch refactor qualita Rust su Windows nativo (D:\\IDEAI): un file per task, gate severo, commit atomici, no auto-merge',
  phases: [
    { title: 'Refactor', detail: 'un agente per file: refactor + gate + commit' },
    { title: 'Checkpoint', detail: 'cargo test workspace ogni N commit' },
  ],
}

const TASK_SCHEMA = {
  type: 'object',
  required: ['file', 'status'],
  properties: {
    file: { type: 'string' },
    status: { type: 'string', enum: ['committed', 'skipped', 'failed'] },
    commit: { type: 'string' },
    reason: { type: 'string' },
    notes: { type: 'string' },
  },
}

const CHECKPOINT_SCHEMA = {
  type: 'object',
  required: ['passed'],
  properties: { passed: { type: 'boolean' }, detail: { type: 'string' } },
}

// RIEMPIRE con i path (forward-slash) della work-list residua, in ordine di priorita.
const FILES = [
  // 'crates/mcp-core/src/playwright_env.rs',
]

const CHECKPOINT_EVERY = 25
const BRANCH = 'quality/win-refactor'
const REPO = 'D:\\IDEAI'

function crateOf(p) {
  const parts = p.split('/')
  return parts[0] === 'crates' && parts[1] ? parts[1] : 'unknown'
}

const files = FILES.map((p) => ({ file: p, crate: crateOf(p) }))
log(`Batch Windows: ${files.length} file su ${REPO} branch ${BRANCH}`)

const results = []
let committed = 0

for (let i = 0; i < files.length; i++) {
  const f = files[i]
  const res = await agent(
    `Sei su WINDOWS. Repo Git nativo in ${REPO} (NON usare WSL, NON path /home/...). Usa SOLO PowerShell.
Per ogni comando lavora con working directory ${REPO}: inizia con \`Set-Location ${REPO}\`.
Lavori SOLO sul file: ${REPO}\\${f.file.replace(/\//g, '\\')}   (crate: ${f.crate})

OBIETTIVO: refactor BEHAVIOR-PRESERVING che riduce i finding di qualita del file.
Lo scanner segnala: funzioni > 50 righe, complessita ciclomatica > 20, categoria security.
- Funzioni > 50 righe: estrai helper privati coesi; non cambiare firme pubbliche ne comportamento osservabile.
- Complessita > 20: estrai sotto-funzioni, usa early-return / match.
- Security: VERIFICA sul codice reale. Se reale, fixa la causa. Se e' un falso positivo del detector regex (es. metodo Rust \`.join(\` scambiato per SQL JOIN, o keyword SQL dentro messaggi d'errore / URL REST), NON forzare un fix: annota con un commento conciso e lascia.
- Niente emoji nel codice/commit. Italiano nei commenti. NON toccare altri file. NON cambiare comportamento, output, o API.

GATE SEVERO (esegui via PowerShell da ${REPO}; se UNO fallisce, ANNULLA con \`git checkout -- ${f.file}\` e riporta status=skipped con la reason):
1. cargo check -p ${f.crate} --message-format=short
2. cargo clippy -p ${f.crate} --all-targets --message-format=short -- -D warnings
3. .\\target\\debug\\xtask.exe quality-scan --gate   (DEVE uscire 0: le metriche globali non devono salire)

Se e solo se i 3 passano, committa (hook lefthook disattivati: girano via bash -lc incompatibile con Windows, e il gate severo qui sopra e' gia' piu' rigoroso):
  $env:LEFTHOOK="0"; git add ${f.file}; git commit -m "refactor(quality): ${f.file} -- riduce long-fn/complexity (behavior-preserving)"
Verifica con \`git log -1 --oneline\` e riporta lo SHA. Se non hai potuto ridurre nulla senza rischi, status=skipped.`,
    { label: f.file.replace(/^crates\//, ''), phase: 'Refactor', schema: TASK_SCHEMA },
  )
  results.push(res || { file: f.file, status: 'failed', reason: 'agente nullo' })
  if (res && res.status === 'committed') committed++

  if (committed > 0 && committed % CHECKPOINT_EVERY === 0 && res && res.status === 'committed') {
    log(`Checkpoint dopo ${committed} commit: cargo test --workspace`)
    const chk = await agent(
      `Sei su WINDOWS, repo ${REPO}. Usa PowerShell, \`Set-Location ${REPO}\`. Esegui:
\`cargo test --workspace --no-fail-fast --message-format=short\`
(Postgres e' il servizio Windows nativo postgresql-x64-17). Riporta passed=true solo
se la suite e' verde. Distingui i fallimenti SOLO di ambiente/DB non disponibile
(riportali nel detail ma considera passed=true) dai fallimenti di logica (passed=false).
NON modificare file.`,
      { label: `checkpoint@${committed}`, phase: 'Checkpoint', schema: CHECKPOINT_SCHEMA },
    )
    if (chk && chk.passed === false) {
      log(`CHECKPOINT ROSSO dopo ${committed} commit: stop. ${chk.detail || ''}`)
      break
    }
  }
}

const committedList = results.filter((r) => r.status === 'committed')
const skipped = results.filter((r) => r.status === 'skipped')
const failed = results.filter((r) => r.status === 'failed')
log(`FINE BATCH: ${committedList.length} commit, ${skipped.length} skip, ${failed.length} fail su ${results.length}`)

return {
  processed: results.length,
  committed: committedList.length,
  skipped: skipped.length,
  failed: failed.length,
  branch: BRANCH,
  details: results,
}
