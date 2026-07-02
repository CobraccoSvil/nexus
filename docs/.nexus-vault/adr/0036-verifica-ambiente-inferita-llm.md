# ADR 0036 — Catena di verifica inferita dall'ambiente via LLM

Data: 2026-07-02 · Stato: implementato (mig 0508)

## Contesto

Incidente Beaty-Book (2026-07-02): il final_gate del run agentico validava le
modifiche con un generico `npm run build`. Per un progetto Vite la build non
esegue il type-check: un run con import/export incoerenti (simboli importati
mai esportati, firme incompatibili) e' stato chiuso "Verifica superata" mentre
il frontend era rotto a runtime al primo click.

La risposta di prima generazione (mig 0503) era una matrice statica
`agent.verify.<lang>.<step>` per 4 linguaggi: non distingue i framework
(Vite vs Next vs CRA), non copre stack non previsti, e ogni nuovo ambiente
richiede una nuova riga di configurazione decisa a priori.

## Decisione

NESSUNA conoscenza d'ambiente fissa (decisione esplicita dell'utente, ribadita
tre volte): niente matrice linguaggio->comando, niente lista di manifest
riconosciuti, niente vocabolario di step, niente comando generico di ripiego.

La catena di verifica di un progetto la decide un LLM che osserva l'ambiente
reale, in due passaggi:

1. **Selezione** (`system.verify_infer.select_files`): dato il listing della
   root (+ primo livello), l'LLM sceglie quali file leggere (max 15, bounded).
2. **Inferenza** (`system.verify_infer.infer_chain`): dai contenuti, l'LLM
   definisce step con NOME LIBERO (`{step, command, working_dir, timeout_s,
   gate, rationale}`), marcando con `gate: true` quelli da eseguire alla
   chiusura di ogni run (rapidi e decisivi) e `gate: false` le verifiche
   profonde on-demand.

Resta fisso SOLO cio' che e' sicurezza o determinismo:

- ogni comando passa dal punto unico `nexus_agent_tools::safety::check_command`
  (mai eseguito un comando bloccato);
- le letture sono confinate alla root (niente `..`, niente assoluti) e bounded;
- l'invalidazione della cache e' deterministica: hash SHA-256 di listing +
  contenuto dei file osservati — un manifest modificato o un file nuovo in
  root rigenera il profilo. Nessun LLM sul percorso caldo.

Se il profilo non esiste e l'LLM non e' raggiungibile, il gate DICHIARA
onestamente "verifica tecnica non eseguita" nella narrazione del run
(meta-step `final_gate`, phase `profile_missing`): mai verificare col comando
sbagliato per dire "verificato".

## Architettura

- **Tabella** `project_verify_profiles` (meta-DB, dominio config progetti):
  `steps` JSONB, `environment` (audit: summary + observed_files),
  `manifest_hash`, `source` (`llm`|`user` — un profilo `user` non viene mai
  sovrascritto dall'inferenza), provider/model usati.
- **Modulo** `mcp-core::verify_profile` — punto unico dell'inferenza
  (`ensure_profile`) e della lettura (`profile_steps`).
- **Purpose** `verify_infer` in `nexus_purpose_model` (tier medium, regola G).
- **final_gate**: `FinalGateConfig.verify_steps` (step `gate=true`, risolti in
  `run_engine` a monte del grafo; in SHADOW sola lettura, mai inferenza) +
  `verify_profile_missing` per la dichiarazione onesta. Rimossi
  `build_command`/`build_working_dir` e le chiavi
  `agent.final_gate.build_command`/`build_check_enabled`.
- **Tool** `nexus_verify_change`: scope `full` = tutti gli step del profilo,
  `quick` = solo i `gate=true`, nome = step specifico. Precedenza comando:
  `run_configurations` (role = nome step, override utente) > profilo.
  Rimossa la matrice statica (DELETE in mig 0508) e la detection di linguaggio.

## Conseguenze

- Un ambiente nuovo (Deno, Flutter, monorepo misto, stack futuri) e' coperto
  senza toccare codice o settings: l'LLM lo osserva e decide.
- Costo: ~2 chiamate LLM per progetto, ripetute solo quando l'ambiente cambia
  (hash). Zero chiamate sul percorso caldo del run.
- La qualita' della catena dipende dal modello del purpose `verify_infer`:
  regolabile dall'admin senza deploy (regola G).
- Il profilo e' ispezionabile e correggibile: `source='user'` blocca
  l'inferenza; le `run_configurations` restano l'override puntuale.

## Riferimenti

- Mig `0508_verify_profile_llm.sql` (tabella, purpose, prompt, pulizia statici)
- `crates/mcp-core/src/verify_profile.rs`
- `crates/nexus-agent-graph/src/nodes/final_gate.rs` (`VerifyStepCmd`)
- ADR 0019 (tool verify), ADR 0018 (criteri strutturali), regole G/H/L/M
