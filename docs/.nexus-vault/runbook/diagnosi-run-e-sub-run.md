---
id: runbook-diagnosi-run-sub-run
kind: runbook
title: "Diagnosticare un run e i suoi sub-run: dove guardare, e cosa mente"
tags: [runbook, diagnosi, troubleshooting, run, subagent, regola-o]
auto_generated: false
created_at: 2026-07-26T00:00:00Z
updated_at: 2026-07-26T00:00:00Z
nexus_meta_version: 1
---

# Diagnosticare un run e i suoi sub-run: dove guardare, e cosa mente

Questo runbook raccoglie le trappole incontrate davvero indagando su run agentici
e sub-agenti. Non sono curiosita': ognuna ha gia' prodotto almeno una diagnosi
FALSA, cioe' un numero che sembrava un fatto ed era un artefatto dello strumento.

E' la regola O applicata alla diagnosi: *lo strumento di misura deve raggiungere
il suo oggetto per la stessa strada della produzione*. Una query che interroga
una colonna diversa da quella che legge il codice non misura il sistema: misura
una sua imitazione, e quando le due divergono non fallisce — risponde con
sicurezza la cosa sbagliata.

## 1. Il nome del tool NON e' in `agent_steps.tool_name`

Quella colonna e' **vuota** (NULL su tutti gli step). Il nome del tool sta dentro
il JSON:

```sql
-- SBAGLIATO: risponde "0 scritture" per QUALUNQUE run
SELECT count(*) FROM agent_steps
 WHERE run_id = $1 AND tool_name IN ('write_file','edit_file');

-- GIUSTO: la stessa strada della produzione
SELECT tool_input->>'tool_name' AS tool, count(*)
  FROM agent_steps WHERE run_id = $1 GROUP BY 1 ORDER BY 2 DESC;
```

La produzione lo legge cosi' in `review_gate_signals`
(`crates/mcp-core/src/chat_messages/agent_run.rs`), che considera scrittura di
codice `write_file | edit_file | create_file | patch_file` e filtra il path con
`is_code_file` (estensioni ts/tsx/js/rs/py/... — un `.md` o un `.json` NON conta).

**Incidente reale (26/07/2026)**: dalla query sbagliata e' uscito "66 step, zero
scritture", e da li' la conclusione "il ReviewGate e' saltato per NoCodeChanges".
Il run aveva invece modificato `frontend/tailwind.config.js`. La conclusione era
falsa e lo strumento avrebbe dato lo stesso responso per ogni run del sistema.

## 2. Un sub-agente e' un run a se'

Ogni sub-run ha la STESSA identita' in due tabelle: una riga in `agent_runs` (con
provider/model propri) e una in `nexus_subagent_runs` (con kind, costo,
provenienza). Le sue tracce sono persistite in `nexus_agent_traces` sotto il
**proprio** `run_id`, non sotto quello del padre.

Conseguenze pratiche:

- un'aggregazione per `run_id` del padre **non vede** il lavoro dei figli (token,
  costo, provider) — vedi ADR 0026, voce "Discendenza di un run";
- vedere comparire un nuovo `agent_runs` mentre un run e' in corso **non**
  significa che l'utente abbia lanciato qualcosa: puo' essere un revisore o una
  figura del consiglio;
- il costo di un run non e' `agent_runs` + basta: i sub-run hanno il proprio
  `cost_usd` in `nexus_subagent_runs`.

## 3. `parent_run_id` e `dispatcher_run_id` rispondono a domande diverse

| colonna | significato | puo' valere |
|---|---|---|
| `parent_run_id` | ancora di FAMIGLIA (`parent_anchor` = `parent_run_id.or(session_id)`): governa depth-chain e cost-cap | anche la **sessione**, che non e' un run |
| `dispatcher_run_id` | il run CORRENTE che ha convocato il figlio | sempre un run, quando il ctx lo porta |

Per la parentela run -> run usare **`dispatcher_run_id`**, tramite il punto unico
`crates/mcp-core/src/run_lineage.rs` (`parent_run_by_child`). Misurato: sub-run
`implement` con `parent_run_id` = sessione e `dispatcher_run_id` = run reale.

Query di controllo — se `padre_e_un_run` e' 0, quel figlio non e' attribuibile ad
alcun run:

```sql
SELECT s.kind, s.dispatcher_run_id,
       (SELECT count(*) FROM agent_runs r WHERE r.id = s.dispatcher_run_id) AS padre_e_un_run
  FROM nexus_subagent_runs s
 WHERE s.created_at > now() - interval '30 minutes';
```

## 4. Le tracce dei sub-run non arrivano in tempo reale

Il canale SSE di un sub-run e' **locale**: alimenta il ponte di narrazione verso
il padre, non viene instradato al frontend (`execute_subagent_run` in
`agent_tools/subagent_native.rs`). Le tracce del figlio compaiono nella UI solo
quando vengono rilette dal DB, cosa che avviene al bootstrap della sessione e
alla chiusura del run.

Quindi: una ripartizione costi che durante il run mostra solo il provider del
padre non e' necessariamente rotta — va guardata a run concluso.

## 5. I run vivono nel DB del PROGETTO, non nel meta

`agent_runs`, `agent_steps`, `nexus_subagent_runs`, `nexus_agent_traces` e
`nexus_agent_meta_steps` stanno nel database per-progetto (cluster app,
`<slug>_nexus`). Nel meta la tabella `agent_runs` **non esiste piu'**: una query
li' non risponde "0 run", risponde "relazione non esistente" — e se l'errore
viene ingoiato, sembra un risultato.

Cercando un run senza sapere il progetto, scandire i database `%_nexus` invece di
assumere quello sbagliato.

## 6. I log ruotano al riavvio, e la loro dimensione mente

Due trappole distinte in `D:\IDEAI-runtime\dev-logs` (timestamp UTC, ora locale
UTC+2):

- dopo un restart il file corrente riparte da zero: **l'assenza di un `run_id`
  nel log non prova che non ci siano stati run**, prova solo che non ce ne sono
  stati *da quel riavvio*. Controllare quante righe ha il file e da quando;
- su NTFS la dimensione riportata puo' essere `0` per file appena scritti: non
  dedurne che il log sia vuoto, leggerne il contenuto.

## Checklist rapida

Dato un run sospetto, in quest'ordine:

1. Esiste? `SELECT id, status, created_at, completed_at FROM agent_runs WHERE id = $1`
   (nel DB del progetto).
2. Cosa ha fatto davvero? `SELECT tool_input->>'tool_name', count(*) FROM agent_steps ... GROUP BY 1`.
3. Sta progredendo o si ripete? Ultimi step per `step_index DESC` con `status`:
   una ripetizione che **fallisce** e' una causa radice da diagnosticare, una che
   **riesce** senza far avanzare nulla e' uno stallo (regola M).
4. Chi ha generato chi? `dispatcher_run_id` (punto 3), mai i meta-step di
   narrazione: sono un canale di presentazione e non tutti i percorsi lo emettono.
5. Quanto e' costato per davvero? Il run **piu'** i suoi discendenti.

## Riferimenti

- Regola O e regola M in `CLAUDE.md`.
- ADR 0026, catalogo dei punti unici (voce "Discendenza di un run").
- `crates/mcp-core/src/run_lineage.rs` — parentela run -> run.
