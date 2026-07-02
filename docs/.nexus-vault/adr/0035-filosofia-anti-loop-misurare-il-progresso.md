# ADR 0035 - Filosofia anti-loop: misurare il progresso, non la ripetizione

Stato: Accettato (implementato)
Data: 2026-07-02

## Contesto

Nel corso del 2026-07-02 sei incidenti reali consecutivi (run c4fa064b,
b833a83d, 2c41b145, 9fa0a6a1 e correlati) hanno mostrato lo stesso pattern:
i meccanismi anti-loop interrompevano run che stavano CONVERGENDO, attribuendo
al modello un'incapacita' che non c'era. La radice comune: decidere su segnali
PARZIALI (firme, contatori, testi) invece che sull'ESITO reale delle azioni.

Il confronto sistematico con il comportamento standard di un agente capace
(Claude Code) ha reso esplicita la differenza di filosofia: un agente capace
non conta le ripetizioni — valuta se l'OUTPUT cambia, cambia STRATEGIA prima
di arrendersi, e dichiara sempre l'esito. Nexus, orchestrando modelli
eterogenei senza umano nel loop, deve ricostruire meccanicamente questi
comportamenti: la scelta di COME farlo e' questa decisione.

## Decisione

**"Stessa azione senza progresso" richiede DUE condizioni: stessa firma E
stesso esito.** Ogni detector di ripetizione deve incorporare un segnale di
progresso prima di dichiarare uno stallo; ogni intervento segue una gerarchia
che imita l'agente capace: correggi -> diagnostica -> CAMBIA STRATEGIA ->
cambia modello -> dichiara l'esito -> chiudi onesto.

### I tre segnali di progresso (deterministici, regola M)

1. **Progresso di esito** (`outputs_similar`, Jaccard su righe, soglia 0.75
   conservativa): un'azione produttiva fallita ripetuta il cui output CAMBIA
   sta progredendo (build che fallisce con errori via via diversi = ciclo
   edit-compila). Consumato da `detect_repeated_action_detailed` (reset del
   conteggio) e dal signature-loop (`repeated_signature_output_progress`).
   NIENTE giudice LLM: "questi due output sono uguali?" si misura, non si
   giudica (costo, latenza e non-determinismo non giustificati).
2. **Progresso di azione**: le letture read-only (inclusi i tool di POLLING
   runtime: read_service_output, tail_service_logs, list_active_services,
   nexus_list_ports) successive a un'azione produttiva sono verifica/
   monitoraggio, non stallo (esclusioni rilettura-dopo-edit e
   rilettura-dopo-progresso; sconto post-produttiva nel signature-loop
   progress-aware).
3. **Progresso di promozione** (grazia post-escalation, `repeat_scan_floor`):
   dopo un cambio di modello i detector contano solo le azioni del promosso —
   il nuovo modello ha diritto ad ALMENO una chiamata prima di qualunque
   nuova decisione.

### La gerarchia di intervento (progress_controller, asse repeated_action)

```
GUIDE            correggi (nudge specifico: copia l'old_string esatto, leggi i log)
FORCE_DIAGNOSE   diagnostica la causa (force-action)
CHANGE_STRATEGY  cambia STRADA restando sul task: strumento alternativo /
                 piu' contesto / passo piu' piccolo (force-action, una tantum)
ESCALATE         cambia modello (budget 3/run, sticky, grazia)
[ADR 0034]       turno dichiarativo forzato: l'esito e' la dichiarazione del modello
ABORT            chiusura onesta (forced_close_unverified -> FailedDiagnosed)
```

Il livello CHANGE_STRATEGY (nuovo con questo ADR) codifica il comportamento
standard dell'agente capace: davanti a uno stallo prima si cambia strada, poi
il cavallo. L'escalation di modello resta necessaria (Nexus orchestra anche
modelli deboli) ma non e' piu' la prima risposta allo stallo.

### Prevenzione prima della reazione (mig 0506)

La sezione `<anti_loop>` dei system prompt agente istruiva ad ARRENDERSI
("se dopo 2 iterazioni non c'e' avanzamento, INTERROMPI e riporta"): riscritta
con la stessa gerarchia dei nudge runtime (cambia UNA cosa -> cambia strategia
-> dichiara blocked con task_complete, mai resa in prosa). La prevenzione nel
prompt agisce PRIMA dello stallo; i nudge del controller restano la rete
quando il modello la ignora.

## Confronto con il comportamento standard di un agente capace

| Caso | Nexus (da questo ADR) | Agente capace (riferimento) |
|---|---|---|
| Identita' azione | firma nome+hash input | conoscenza nel contesto |
| Build/test ripetuti | loop solo se l'ESITO non cambia | identico |
| Riletture/polling | scontate dopo il progresso | contesto gia' noto |
| Descrive senza agire | contatore G1 + nudge | istruzione permanente nel prompt |
| Stallo persistente | CHANGE_STRATEGY poi escalation | cambia approccio, poi chiede aiuto |
| Dichiarazione esito | task_complete strutturato (ADR 0034) | resoconto a ogni turno |
| Verifica del "fatto" | final_gate obbligatorio (build/test) | autodisciplina + prompt |
| Rete finale | iteration_cap (ora DB-driven) + recursion_limit | context window + utente |

Differenza legittima e permanente: Nexus e' unattended e multi-modello, quindi
i tetti duri (budget escalation 3, iteration_cap, max_cycles del final_gate)
restano necessari come ultima rete; l'agente capace ha l'utente nel loop.

## Conseguenze

- Ogni futuro detector di ripetizione DEVE rispondere alla domanda "gli esiti
  delle occorrenze erano uguali?" prima di dichiarare stallo (checklist per i
  falsi positivi: 1. il tool e' in EXPLORATION_ONLY_TOOLS? 2. e' polling?
  3. gli output erano diversi? 4. c'era lavoro produttivo in mezzo?).
- `agent.executor.iteration_cap` e' DB-driven (era una costante dichiarata
  configurabile ma mai letta — incoerenza chiusa con la mig 0506).
- Le soglie ancora costanti nel codice (LOOP_THRESHOLD=3, finestra 6, budget
  escalation 3, OUTPUT_SIMILARITY_THRESHOLD=0.75) restano tali finche' non
  emerge un bisogno reale di tuning: promuoverle a settings solo su richiesta
  (niente over-engineering).
- La narrazione live (meta-step `strategy_shift`, `escalation`, `loop_break`,
  `declaration_request`, `outcome_declared`, `final_gate`) rende ogni
  intervento della gerarchia VISIBILE in chat: l'utente vede perche' il run
  cambia strada, modello o si ferma.

## Riferimenti

- ADR 0033 (classificazione errori deterministica), ADR 0034 (esito
  dichiarato strutturato), mig 0502/0504/0506
- Incidenti: run c4fa064b (escalation bruciata in 150ms), b833a83d
  (rilettura di debugging uccisa), 2c41b145 (polling servizi), "npm run
  build si ripeteva" (esiti diversi ignorati)
- Regole CLAUDE.md: G (fonte unica), H (fix definitivo), L (punto unico),
  M (segnali strutturati)
