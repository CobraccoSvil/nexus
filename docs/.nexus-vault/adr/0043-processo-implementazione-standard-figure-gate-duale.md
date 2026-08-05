---
id: adr-0043-processo-implementazione-standard-figure-gate-duale
kind: adr
title: "ADR 0043 - Processo di implementazione standard per le figure e gate duale sui passi critici"
slug: 0043-processo-implementazione-standard-figure-gate-duale
tags:
  - adr
  - figure
  - processo-implementazione
  - gate-duale
  - prompt
  - hitl
  - regola-L
  - regola-M
  - regola-Q
auto_generated: false
created_at: 2026-08-05T00:00:00Z
updated_at: 2026-08-05T00:00:00Z
nexus_meta_version: 1
---

# ADR 0043 - Processo di implementazione standard per le figure e gate duale sui passi critici

## Stato

Accepted - 2026-08-05. Implementata in 5 wave sul branch
`feature/processo-standard-figure` (commit finali `d956fd7c` e `0cf02e2c`),
migrazioni META 0674-0678. Non ancora mergiata in `main`.

## Contesto

Il template utente "NEXUS - Prompt di Implementazione" descriveva un processo di
lavoro in 5 fasi — **analisi, progettazione, implementazione, verifica,
chiusura** — con taglia del task (S/M/L), criteri di accettazione **eseguibili**
e una Definition of Done esplicita. Era un documento per umani: nessuna figura
agente lo seguiva, e ogni figura aveva il proprio protocollo implicito, diverso
dagli altri e non verificabile.

Questa decisione lo promuove a **processo operativo di TUTTE le figure agente**,
con quattro requisiti aggiuntivi che il template per umani non poteva imporre:

1. **Passi critici validati da DUE provider AI distinti** piu' un controllo
   avversariale: un solo giudice condivide i bias del proprio modello, e un
   giudice che gira sullo stesso provider dell'esecutore non e' un giudice
   (stesso principio del veto "giudice != worker").
2. **Contesto vettorializzato dove possibile**: una figura convocata su un
   mandato non deve riscoprire da zero cio' che il progetto gia' sa.
3. **Validazione in ogni fase**, non solo alla chiusura: un errore di analisi
   scoperto al final_gate costa l'intero run.
4. **Saldo strati <= 0**: il blocco nuovo non si AGGIUNGE alle stratificazioni
   esistenti, le SOSTITUISCE. Le stratificazioni storiche (mig 0064, mig 0362,
   protocollo coder) sono assorbite nella stessa migrazione che introduce il
   blocco — mai due testi che dicono la stessa cosa in due punti del prompt
   (regola L applicata al prompt stesso).

## Decisione

Cinque wave, una migrazione ciascuna, ognuna con kill-switch documentato in
testa alla migrazione.

### W0 - Il processo come blocco unico di prompt (mig 0674)

Il blocco `<processo_implementazione>` vive in **UN** template DB
(`system.implementation_process`) e viene innestato dai **TRE** compositori di
system prompt — chat, run agentico, sub-run — attraverso il punto unico
`crates/mcp-core/src/prompt_processo.rs`. Nessun compositore incolla il testo
per conto suo: chi lo facesse riprodurrebbe il difetto che questa wave assorbe.

- **Le figure advisory NON lo ricevono.** Il discriminante e' il punto unico
  `is_advisory_kind`: chi emette un parere non implementa, e dargli un processo
  di implementazione sarebbe rumore che invita a scrivere (lezione del
  Consiglio che convocava figure che scrivono).
- **La degradazione e' per MANDATO nel testo** (implementazione / verdetto /
  analisi), mai un secondo discriminante a codice: due discriminanti per la
  stessa domanda sono due punti di verita' (regola L).
- **Posizione: parte STABILE del prompt.** Il blocco entra prima del
  `CONFINE_DI_TURNO`, cosi' la cache del prefisso resta intatta: un blocco
  ricalcolato in testa non fallisce nulla, e' corretto in tutto tranne che nel
  prezzo (vedi punto unico `composizione-system-prompt`).

### W1 - Criteri di accettazione eseguibili (mig 0675, attiva al deploy)

Un criterio di accettazione che non si puo' eseguire e' una dichiarazione, non
un criterio (regola M).

- **Vocabolario UNICO dei tipi di criterio**, derivato dal dispatch REALE del
  `criteria_runner`: `run_command | http | file_exists`. Non un vocabolario
  disegnato a tavolino e poi mappato: se il runner non sa eseguirlo, il tipo non
  esiste (regola O — lo strumento raggiunge il suo oggetto come la produzione).
- **Schema strict sul tool `nexus_todo_write`** come controllo agentico alla
  fonte: il modello che scrive un todo con criteri dichiara tipi validi per
  costruzione. **NESSUN rifiuto in `create_plan`**: il planner e' una chiamata
  one-shot senza loop di correzione, e un rifiuto li' sarebbe un run morto, non
  un criterio migliore.
- **Degrado tipo-ignoto a `Inconclusive` RISTRETTO ai criteri authored.** Ogni
  criterio porta la propria provenienza (`Gate | Authored`): un tipo ignoto in
  un criterio scritto dal modello e' un "non ho potuto verificare" onesto; lo
  stesso degrado su un criterio del gate maschererebbe un difetto del gate.
- **Flip del verifier**: `todo_criteria_mode` passa da `observe` a `enforce`.

### W2 - Approvazione del piano, docs nel DoD, sonda anti-SPA (mig 0676)

- **Gate di approvazione del piano in Confirm >= medium**: pending action
  `plan_approval` con la copertura dei criteri in vista; `approved_at` /
  `approved_by` su `nexus_agent_plans`. L'approvazione del PIANO **non
  pre-approva i mutatori concreti**: approvare "aggiungero' una colonna" non e'
  approvare la `ALTER TABLE` che poi arriva — i due consensi restano distinti.
- **Docs nella Definition of Done**: `task_complete` acquisisce il campo
  `docs_updated` (`updated | not_needed | missing`), verificato dal final_gate
  con un criterio strutturale claim-vs-fatti: la dichiarazione del modello resta
  una dichiarazione finche' i fatti (file docs toccati) non la confermano
  (regola M, corollario della Q).
- **Sonda HTTP che rifiuta il fallback SPA**: per gli endpoint API la sonda
  pretende un Content-Type non-HTML. Un proxy rotto mascherato da un 200 con
  `index.html` non e' un endpoint che risponde — e' il difetto misurato sui
  cicli E2E, promosso a criterio.

### W3 - Gate duale sui passi critici (mig 0677)

Il cuore della decisione: un passo critico non si esegue sulla parola
dell'esecutore.

- **Classificazione in-memory da regole DB** (`orchestrator.critical_step_rules`)
  nel punto unico `nexus-agent-graph/src/decisions/step_gate.rs`. Il matcher e'
  a **TOKEN fuori-quote**: un comando che NOMINA `rm -rf` (in una stringa, in un
  commento, in un `echo`) non lo esegue, e un matcher a `contains` non distingue
  i due casi (lezione del commit `7cb4fd14`).
- **Per i livelli in enforcement, DUE chiamate one-shot su provider distinti**
  fra loro E dall'esecutore. Adapter:
  `mcp-core/src/agent_graph_adapter/step_validation.rs`, con l'identita'
  contabile del run (ogni chiamata e' addebitata a chi la causa). Il verdetto
  arriva dal tool `step_verdict` inline con `tool_choice` forzato e si legge
  **SOLO dai campi**, mai dalla prosa (regole M e Q, ADR 0034).
- **Mandati asimmetrici**: un **gatekeeper** neutro ("questo passo e' corretto
  e sicuro?") e un **challenger** refutativo ("trova il motivo per cui questo
  passo e' sbagliato"). Due giudici con lo stesso mandato sono un giudice
  contato due volte.
- **Matrice dei verdetti**: unanimita' approva; un reject rimanda all'esecutore
  coi motivi; il disaccordo sospende in HITL **anche in Automatic**;
  l'astensione non e' MAI un si' — il denominatore sono i convocati, non i
  rispondenti (stesso principio del quorum onesto del Consiglio).
- **Mode**: `off | observe | enforce_irreversible | enforce`, default
  `enforce_irreversible`.
- **Il difetto fatale trovato dalla review avversaria pre-commit**: il
  corto-circuito su `state.approved` — seminato `true` in Automatic proprio per
  saltare l'HITL — spegneva il gate esattamente dove il gate era l'unica
  barriera rimasta. Corretto sostituendolo con un marker di BATCH (id ordinati,
  `step_gate_cleared_batch`): un'approvazione copre i passi che ha visto, non i
  successivi. Senza la review avversaria il gate sarebbe stato committato come
  funzionante con test verdi (stessa lezione dell'ADR 0040: compilare e passare
  i test non dice che il percorso sia raggiungibile).
- **Sub-run col gate armato**: l'apply del worktree copre solo le mutazioni
  file; uno stop servizi o un kill dentro un sub-run non passa da li', quindi il
  gate deve valere anche dentro i sub-run.
- **La sospensione in Automatic ha una SCADENZA (rilievo A4, mig 0679 +
  project 0016).** Sospendere in HITL dove nessun umano esiste e' il punto del
  requisito, ma nessun apparato raccoglieva quel run: il `run_reaper` esclude
  `awaiting_confirmation` per contratto (mig 0392: e' resumibile via
  checkpoint, ucciderlo distruggerebbe lavoro) e `ACTIVE_RUN_STATUSES` lo conta
  fra i run che OCCUPANO la sessione. Un run notturno con validatori discordi
  restava appeso per sempre e ingorgava la sessione: al mattino non c'era un
  esito da leggere, non c'era niente. Ora la sospensione porta un termine, e a
  termine maturo il run chiude con esito STRUTTURATO `blocked_needs_input` +
  `blocker` derivato dal kind — mai `interrupted`, che direbbe "e' morto
  qualcosa" di un run fermatosi esattamente dove doveva.
  - **Il discriminante e' la MODALITA', non l'origine della sospensione.** Le
    due sospensioni HITL ordinarie (tool mutativi, approvazione del piano)
    pretendono entrambe Confirm per nascere, cioe' nascono solo dove un umano
    e' al terminale; il gate duale e' l'unico che sospende dove non c'e'
    nessuno. Un criterio scritto sull'origine avrebbe coperto il caso di oggi
    lasciando scoperta la prossima sospensione che nascesse in Automatic. In
    Confirm nessuna sospensione scade: una scadenza chiuderebbe un run che
    l'utente stava per approvare.
  - **La scadenza non poteva venire dal solo budget residuo del run**, come il
    piano indicava: `agent.run_time_budget_s` vale `0` per policy dichiarata
    (mig 0604/0607) — misurato sul DB vivo — quindi per il run PRIMARIO, che e'
    il caso del difetto, `run_time_remaining_s` ritorna `None`. Un fix derivato
    solo da li' sarebbe stato reale nel codice e irraggiungibile nei dati. La
    fonte e' una chiave dedicata; il residuo resta il TETTO dove esiste (i
    sub-run un budget ce l'hanno).
  - **La chiusura revoca l'approvazione tardiva**: l'handler `approve` pretende
    `awaiting_confirmation` e risponde 409 su ogni altro stato, quindi un
    consenso che arrivi a run chiuso non fa ripartire il passo irreversibile.

### W4 - Recall del contesto nei mandati (mig 0678)

- Blocco `<contesto_richiamato>` costruito con il `rag::search_semantic`
  ESISTENTE (nessun secondo motore di ricerca): fonte + score per ogni chunk,
  **fail-open** (un recall fallito non blocca il mandato), timeout complessivo
  5s, chunk sanificati col punto unico `sanitize_for_system_block`.
- Innestato in `prepare_subagent_run` **nel MESSAGGIO**, non nel system: il
  contesto richiamato dipende dal mandato e cambia a ogni convocazione — nel
  system romperebbe la cache del prefisso (stesso razionale della posizione
  delle memorie di progetto).
- Direttiva `<recupero_contesto>` nel system delle SOLE figure che hanno tool
  semantici in whitelist: ordinare a una figura di cercare con un tool che non
  ha e' la stessa forma di difetto del blocco `<privilegi_sistema>` che
  ordinava `apt-get` su Windows.

## Conseguenze

- **Costo**: ~2 chiamate LLM per passo critico, ma solo per i livelli
  Critical/Irreversible in enforcement; con il default `enforce_irreversible` la
  maggioranza dei passi non paga nulla.
- **Attrito**: fino a 3 frizioni in Confirm high (approvazione piano, HITL sui
  mutatori, HITL su disaccordo del gate). Scelta deliberata: rigore > attrito —
  chi sceglie Confirm high sta chiedendo esattamente questo.
- **Taratura prima dell'enforcement pieno**: il meta_step `step_validation`
  rende osservabile il kind (quali passi vengono classificati, con quali
  verdetti) in modalita' observe. Il passaggio a `enforce` pieno avviene SOLO
  via migrazione dedicata, mai con un UPDATE operativo (regola H: un UPDATE a
  mano e' una toppa; la migrazione e' la decisione versionata).
- **Reversibilita'**: ogni migrazione documenta il proprio kill-switch in testa
  al file. Spegnere una wave e' un'operazione dichiarata, non un revert cieco.

## Riferimenti

- Migrazioni META `0674` (blocco processo), `0675` (criteri eseguibili),
  `0676` (approvazione piano + docs DoD + sonda anti-SPA), `0677` (gate duale),
  `0678` (recall contesto), `0679` (sorveglianza delle sospensioni, rilievo A4);
  migrazione PROJECT `0016` (colonne `suspension_kind` /
  `suspension_expires_at` su `agent_runs`).
- Branch `feature/processo-standard-figure`, commit finali `d956fd7c` e
  `0cf02e2c`.
- Moduli punto unico: `crates/mcp-core/src/prompt_processo.rs`,
  `crates/nexus-agent-graph/src/decisions/step_gate.rs`,
  `crates/mcp-core/src/agent_graph_adapter/step_validation.rs`.
- [[0026-punto-unico-de-duplicazione]] — catalogo dei punti unici (regola L).
- [[0034-esito-conversazione-strutturato-finish-task]] — esiti strutturati, mai
  dalla prosa.
- [[0040-orchestrazione-dimensionata-dal-problema]] — mandati asimmetrici e
  lezione "test verdi su codice irraggiungibile".
- Regole CLAUDE.md: G (config nel DB), H (fix definitivi), L (punti unici),
  M (segnali strutturati), N (identificatori canonici), O (la misura raggiunge
  il suo oggetto), Q (l'esito in un campo).
