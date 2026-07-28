// Batch refactor qualita Rust su Windows nativo.
// IMPORTANTE: lanciare SOLO da una sessione Claude Code con working directory su
// un path Windows reale. Da una sessione con cwd UNC \\wsl.localhost i subagenti
// NON eseguono shell (vedi memoria reference_subagenti_cwd_unc_windows).
//
// Lavora su un WORKTREE ISOLATO (REPO qui sotto), non sul tree condiviso D:\IDEAI:
// due flussi che scrivono lo stesso tree = race/sovrascritture.
//
// La lista FILES va INCORPORATA qui sotto (NON passata via args: args non viene
// iniettato quando si passa lo script inline). Per rigenerarla:
//   .\target\debug\xtask.exe quality-scan --export %TEMP%\wl.json
// e ordinare per impatto reale (long_functions*10 + complexity*8).

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

// Work-list giro-4 (2026-07-14): rientro del debito entrato negli 89 commit tra
// 443482a8 e HEAD, misurato con la metrica NUOVA su entrambe le sponde
// (xtask quality-scan --root <worktree base443>): long-fn 743 -> 806 (+63),
// complexity 14 -> 16 (+2), security invariata a 44.
// Ordinata per IMPATTO REALE (long_functions*10 + complexity*8), NON per il campo
// `priority` del JSON che sovrappesa la security (molti security sono falsi
// positivi del detector regex -> un file solo-security manda l'agente in skip).
// Escluso il super-hotspot agent_run.rs (14 lf / 1 cx, zone di PERSISTENZA):
// intervento dedicato, non batch automatico.
// Prerequisito VERIFICATO: cache clippy workspace calda (clippy -p mcp-core: 0.5s).
const FILES = [
  'crates/mcp-core/src/project_workspace/services.rs', // lf=18 cx=1
  'crates/nexus-agent-graph/src/nodes/executor.rs', // lf=15 cx=1
  'crates/mcp-core/src/orchestrator/core.rs', // lf=12 cx=1 (cx NUOVO)
  'crates/mcp-core/src/chat_messages/handlers.rs', // lf=10 cx=1
  'crates/mcp-core/src/agent_tools/subagent_native.rs', // lf=10 cx=0
  'crates/mcp-core/src/native_engine.rs', // lf=10 cx=0
  'crates/mcp-core/src/project_db_routes/connection.rs', // lf=7 cx=1
  'crates/mcp-core/src/projects/indexing.rs', // lf=7 cx=0
  'crates/mcp-core/src/wiki/routes.rs', // lf=7 cx=0
  'crates/nexus-agent-graph/src/decisions/context_reduction.rs', // lf=6 cx=1
  'crates/mcp-core/src/projects/deep_analyze.rs', // lf=6 cx=0
  'crates/mcp-core/src/learned_instructions.rs', // lf=6 cx=0
  'crates/mcp-core/src/nexus_builtin/mcp_runtime.rs', // lf=6 cx=0
  'crates/mcp-core/src/project_workspace/wizard.rs', // lf=6 cx=0
  'crates/nexus-agent-tools/src/gateway_client.rs', // lf=6 cx=0
  'crates/nexus-gateway/src/server/routes.rs', // lf=6 cx=0
  'crates/nexus-wiki/src/triple_extractor.rs', // lf=6 cx=0
  'crates/mcp-core/src/chat_agent.rs', // lf=5 cx=1 (cx NUOVO)
  'crates/mcp-core/src/plugins/mod.rs', // lf=5 cx=1
  'crates/mcp-core/src/agent_graph_adapter/criteria_runner.rs', // lf=5 cx=0
  'crates/mcp-core/src/chat_sessions.rs', // lf=5 cx=0
  'crates/mcp-core/src/port_registry.rs', // lf=5 cx=0
]

const CHECKPOINT_EVERY = 8
const BRANCH = 'claude/charming-zhukovsky-305a4e'
const REPO = 'D:\\IDEAI-worktrees\\charming-zhukovsky-305a4e'

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

VIETATO BARARE COL GATE (regola H di CLAUDE.md — sarebbe una toppa, non un fix):
- Lo scanner ESCLUDE dai conteggi i moduli \`#[cfg(test)]\` inline. NON spostare codice
  di PRODUZIONE dentro \`#[cfg(test)]\` per far scendere la metrica: e' barare.
- NON marcare codice vivo con \`#[cfg(test)]\`, \`#[allow(...)]\` o cfg-flag per nasconderlo.
- NON cancellare codice vivo per far scendere il conteggio.
- La riduzione deve venire SOLO da estrazione reale di helper coesi e riuso.
- Se il file non e' riducibile senza rischio o senza barare, status=skipped e' un ESITO
  LEGITTIMO e preferibile a un refactor forzato.

REGOLA L (punto unico): se estraendo un helper noti che la stessa logica esiste gia'
altrove, NON duplicarla: se il consolidamento e' fuori scope per questo file, annotalo
in \`notes\` invece di crearne una seconda copia.

Nota clippy: gli helper con >7 parametri fanno scattare \`clippy::too_many_arguments\`
(fatale con -D warnings): tieni gli helper a <=6 parametri, o passa una struct di contesto.

GATE SEVERO (esegui via PowerShell da ${REPO}; se UNO fallisce, ANNULLA con \`git checkout -- ${f.file}\` e riporta status=skipped con la reason):
1. cargo check -p ${f.crate} --message-format=short
2. cargo clippy -p ${f.crate} --all-targets --message-format=short -- -D warnings
3. .\\target\\debug\\xtask.exe quality-scan --gate   (DEVE uscire 0: le metriche globali non devono salire)

Se e solo se i 3 passano, committa. NOTA: in un worktree l'hook pre-commit lefthook NON
si risolve e ritorna 0 senza eseguire nulla (falso verde silenzioso) -> i gate NON girano
da soli, per questo il gate severo qui sopra e' esplicito e va eseguito DAVVERO:
  $env:LEFTHOOK="0"; git add ${f.file}; git commit -m "<messaggio>"
Messaggio di commit: prima riga \`refactor(quality): <cosa e' cambiato nel file>\`, poi una
riga vuota e 2-4 righe che dicono QUALI funzioni hai spezzato e in quali helper, e perche'
il comportamento e' preservato. Niente emoji. Descrivi il cambiamento reale, non "riduce i
finding": il numero e' la conseguenza, non lo scopo.
Verifica con \`git log -1 --oneline\` e riporta lo SHA.
Se non hai potuto ridurre nulla senza rischi o senza barare, status=skipped con la reason.`,
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
