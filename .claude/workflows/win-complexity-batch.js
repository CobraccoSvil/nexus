// Batch mirato sulle funzioni con complessita ciclomatica > 20 (gate quality-scan).
// Un agente per FUNZIONE (non per file): ogni task ha un bersaglio unico, noto e misurato.
//
// Lanciare da una sessione con cwd su un path Windows reale, su un WORKTREE ISOLATO.
// Prerequisito: cache clippy calda (`cargo clippy --workspace --all-targets`).
// Il file deve restare a fine-riga LF: con CRLF il lancio viene rifiutato.

export const meta = {
  name: 'win-complexity-batch',
  description: 'Riduce sotto 20 le funzioni con complessita ciclomatica alta: una funzione per task, gate severo, commit atomici',
  phases: [
    { title: 'Refactor', detail: 'un agente per funzione: estrae helper + gate + commit' },
  ],
}

const TASK_SCHEMA = {
  type: 'object',
  required: ['fn', 'status'],
  properties: {
    fn: { type: 'string' },
    status: { type: 'string', enum: ['committed', 'skipped', 'failed'] },
    complexity_before: { type: 'number' },
    complexity_after: { type: 'number' },
    commit: { type: 'string' },
    reason: { type: 'string' },
    notes: { type: 'string' },
  },
}

// Le 10 funzioni fattibili, misurate replicando il detector reale (validato: riproduce
// esattamente riga e nome dei finding). Ordinate per complessita crescente = rischio
// crescente. ESCLUSI i 2 super-hotspot, che sono di categoria diversa e vogliono un
// intervento dedicato con verifica E2E:
//   - nexus-agent-graph/src/nodes/executor.rs::run          cx=180 (cuore del motore)
//   - mcp-core/src/chat_messages/agent_run.rs::spawn_agent_run cx=120 (persistenza)
//
// GIRO 2: i 4 restanti. I primi 6 sono gia' committati (23c2dba1..42a02821, complexity
// 12 -> 6) e verificati: cargo test --workspace 2981 passed / 0 failed.
// Il checkpoint intermedio del giro 1 e' stato RIMOSSO: aveva dato un falso rosso fermando
// il batch a 6/10. Non aveva trovato test falliti — non era riuscito a ESEGUIRLI, perche'
// usava CARGO_TARGET_DIR=target-verify (freddo: la compilazione da zero non rientrava nella
// finestra dell'agente). Ha riportato passed=false = "assenza di evidenza", che e' corretto
// (regola M), ma il segnale non era un fallimento di logica. La suite si esegue una volta a
// fine batch, dal chiamante, sul target caldo.
const TARGETS = [
  { file: 'crates/nexus-build-graph/src/resolver_typescript.rs', fn: 'resolve_typescript', line: 42, cx: 28 },
  // 22 dei 30 punti vengono da funzioni ANNIDATE nel corpo: estrarle a livello modulo
  // e' la leva principale qui, e abbatte la complessita senza toccare la logica.
  { file: 'crates/mcp-core/src/project_db_routes/config.rs', fn: 'set_project_db_config', line: 124, cx: 30, nested: 22 },
  { file: 'crates/nexus-agent-tools/src/scaffold_verifier.rs', fn: 'tool_nexus_verify_scaffold', line: 37, cx: 38 },
  { file: 'crates/nexus-agent-graph/src/nodes/tool_dispatch.rs', fn: 'run', line: 607, cx: 52 },
]

const REPO = 'D:\\IDEAI-worktrees\\charming-zhukovsky-305a4e'

function crateOf(p) {
  const parts = p.split('/')
  return parts[0] === 'crates' && parts[1] ? parts[1] : 'unknown'
}

log(`Batch complessita: ${TARGETS.length} funzioni su ${REPO}`)

const results = []
let committed = 0

for (let i = 0; i < TARGETS.length; i++) {
  const t = TARGETS[i]
  const crate = crateOf(t.file)
  const winPath = t.file.replace(/\//g, '\\')
  const nestedHint = t.nested
    ? `\nLEVA PRINCIPALE per questa funzione: ${t.nested} dei ${t.cx} punti vengono da FUNZIONI ANNIDATE definite dentro il corpo. Il detector conta le loro keyword come se fossero della host: estrarle a livello modulo abbatte la complessita senza toccare la logica. Parti da li'.`
    : ''

  const res = await agent(
    `Sei su WINDOWS. Repo Git nativo in ${REPO} (NON usare WSL, NON path /home/...). Usa SOLO PowerShell.
Per ogni comando lavora con working directory ${REPO}: inizia con \`Set-Location ${REPO}\`.

BERSAGLIO UNICO: la funzione \`${t.fn}\` in ${REPO}\\${winPath} (intorno a riga ${t.line}, crate: ${crate}).
Complessita ciclomatica attuale misurata: ${t.cx}. Soglia del gate: > 20 = finding "high".
OBIETTIVO: portarla a <= 20 con un refactor BEHAVIOR-PRESERVING. NON toccare altre funzioni ne altri file.${nestedHint}

COME E' CALCOLATA LA METRICA (leggi bene: rifattorizzare senza saperlo e' tirare a indovinare).
Il detector (crates/mcp-quality/src/lib.rs, fn check_complexity) e' LINE-BASED e testuale:
  complessita = 1 + numero di occorrenze della regex \\b(if|else\\s+if|elif|while|for|match|case|catch|&&|\\|\\|)\\b
  su OGNI RIGA del corpo della funzione, delimitato contando le graffe.
Conseguenze pratiche:
- ATTENZIONE ai \`\\b\` nella regex: attorno a \` && \` / \` || \` SPAZIATI non c'e' word boundary
  (spazio e \`&\` sono entrambi non-word), quindi in Rust idiomatico gli operatori booleani
  NON contano MAI. Non perdere tempo a collassarli: vale zero. (E' un falso NEGATIVO del
  detector: la complessita reale e' piu' alta di quella misurata. Il posto giusto per
  correggerlo e' il detector, non le funzioni misurate: non toccarlo qui.)
- le keyword dentro le funzioni ANNIDATE nel corpo contano per la funzione host: estrarre
  una fn annidata a livello modulo sposta via tutti i suoi punti;
- estrarre un blocco in un helper a livello modulo sposta i suoi punti nell'helper;
- NON fidarti di questa descrizione a scatola chiusa: replica \`check_complexity\` in uno
  script e MISURA il bersaglio prima e dopo. Se il conto non torna con la misura dichiarata,
  ha ragione la tua replica: dillo in \`notes\`.
Commenti e stringhe: su questa funzione contribuiscono ~0, NON perdere tempo a riformularli
(sarebbe anche un workaround: il posto giusto per un falso positivo e' il detector).

VINCOLO CRITICO — non auto-sabotarti (il gate controlla anche il TOTALE dei finding):
- un finding "complexity" nasce gia' sopra 10 (severity medium) e conta nel TOTALE;
- una funzione > 50 righe crea un finding "long function" e conta nel TOTALE;
=> OGNI helper che estrai deve stare SOTTO 10 di complessita E SOTTO 50 righe, altrimenti
   crei nuovi finding, il totale sale e il gate diventa ROSSO anche se la complessita scende.
   Preferisci piu' helper piccoli e coesi a un unico helper grosso.

VIETATO BARARE COL GATE (regola H di CLAUDE.md — sarebbe una toppa, non un fix):
- lo scanner ESCLUDE i moduli \`#[cfg(test)]\` inline: NON spostare codice di produzione li' dentro;
- NON usare #[allow(...)] / cfg-flag per nascondere codice vivo, NON cancellare codice vivo;
- la riduzione deve venire SOLO da estrazione reale di helper coesi.
Se la funzione non e' riducibile senza rischio o senza barare, status=skipped e' un ESITO
LEGITTIMO e preferibile a un refactor forzato: dillo nella reason.

Regola L: se estraendo noti che la stessa logica esiste gia' altrove, NON duplicarla; se il
consolidamento e' fuori scope, annotalo in \`notes\`.
Niente emoji. Italiano nei commenti. Helper con <= 6 parametri (clippy::too_many_arguments
e' fatale con -D warnings): se servono piu' dati, passa una struct di contesto.

GATE SEVERO (esegui via PowerShell da ${REPO}; se UNO fallisce, ANNULLA con
\`git checkout -- ${t.file}\` e riporta status=skipped con la reason):
1. cargo check -p ${crate} --message-format=short
2. cargo clippy -p ${crate} --all-targets --message-format=short -- -D warnings
3. .\\target\\debug\\xtask.exe quality-scan --gate   (DEVE uscire 0: nessuna metrica globale sale)
Verifica anche di aver COLPITO il bersaglio: dopo il refactor \`${t.fn}\` non deve piu' comparire
tra i finding complexity-high. Controlla cosi':
  .\\target\\debug\\xtask.exe quality-scan --export "$env:TEMP\\cx_check.json"
poi cerca "${t.fn}" nel JSON: se e' ancora li' con category=complexity e severity=high, NON hai
finito — continua a estrarre. Riporta complexity_before e complexity_after (dal campo detail
del finding, formato "Complexity: N").

Se e solo se il gate passa E il bersaglio e' colpito, committa (in un worktree l'hook lefthook
non si risolve e ritorna 0 senza eseguire nulla: i gate qui sopra sono il vero controllo):
  $env:LEFTHOOK="0"; git add ${t.file}; git commit -m "<messaggio>"
Messaggio: prima riga \`refactor(complessita): <cosa hai spezzato e in cosa>\`, poi riga vuota e
2-4 righe che dicono quali helper hai estratto e perche' il comportamento e' preservato.
Descrivi il cambiamento reale, non "riduce i finding": il numero e' la conseguenza, non lo scopo.
Verifica con \`git log -1 --oneline\` e riporta lo SHA.`,
    { label: `${t.fn} (cx=${t.cx})`, phase: 'Refactor', schema: TASK_SCHEMA },
  )

  results.push(res || { fn: t.fn, status: 'failed', reason: 'agente nullo' })
  if (res && res.status === 'committed') committed++
}

const ok = results.filter((r) => r.status === 'committed')
const skipped = results.filter((r) => r.status === 'skipped')
const failed = results.filter((r) => r.status === 'failed')
log(`FINE BATCH: ${ok.length} commit, ${skipped.length} skip, ${failed.length} fail su ${results.length}`)

return {
  processed: results.length,
  committed: ok.length,
  skipped: skipped.length,
  failed: failed.length,
  details: results,
}
