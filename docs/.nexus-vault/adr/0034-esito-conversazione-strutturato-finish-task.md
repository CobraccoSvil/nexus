# ADR 0034 - Esito conversazione strutturato (tool task_complete)

Stato: Implementato (2026-07-02; design originario 2026-07-01)
Data: 2026-07-01

## Contesto

Con ADR 0033 la classificazione dell'errore PROVIDER/trasporto e' deterministica
(status HTTP + codice strutturato, mai la prosa). Restava un secondo punto che
usava euristiche testuali: la determinazione dell'ESITO della conversazione col
modello (il "il modello non riesce"), rilevata via pattern lessicali
(`detect_unfulfilled_intent`, `resigned_patterns` nel bridge legacy): fragile per
lingua/parafrasi, esattamente come lo era la classificazione errori.

Ricerca (RFC 9457 Problem Details; structured outputs / constrained decoding;
pattern Pydantic/Instructor) indica la via deterministica: far dichiarare al
modello il proprio esito in un OUTPUT STRUTTURATO garantito da schema.

## Scoperta in fase di implementazione (root cause reale)

Il tool di dichiarazione esisteva gia' (`task_complete`, WAVE 3): handler nel
grafo nativo (`nodes/tool_dispatch.rs`), normalizzazione
(`decisions/tool_dispatch.rs::normalize_declared_outcome`), consumo nel routing
(`declared_outcome_kind`, gate G1). MA la sua DEFINIZIONE non era in nessun
catalogo esposto al modello: era un tool del brain Python (rimosso, commit
75a6d62) mai portato in `AGENT_TOOLS_JSON`. Verifica empirica: zero chiamate a
`task_complete` in tutti i run del motore nativo su `beaty_book_nexus`. Il
"segnale primario" della catena documentata in mig 0422 (task_complete ->
segnali strutturali -> closure_judge -> blacklist lessicale) era di fatto morto:
si cadeva SEMPRE alle euristiche.

## Decisione (as-built)

Il tool si chiama `task_complete` (nome storico WAVE 3, non `finish_task`:
handler e consumo esistevano gia' — regola L, un solo punto).

1. **Definizione nel catalogo** (`crates/nexus-agent-tools/src/tool_schema.rs`,
   fonte unica degli schema) con lo schema esteso:

```
task_complete(
  outcome:  enum["done","blocked","partial","needs_input"],  // codice macchina
  summary:  string,                                          // umano, solo display
  next_step?: string,
  blocked_by?: string,                                       // umano, solo display
  blocker?: enum["dependency","credential","permission",
                 "service","request_ambiguity","safety"],    // codice macchina
  refusal?: boolean,                                         // rifiuto safety
  files_touched?: string[]                                   // self-report
)
```

2. **Esposizione via whitelist DB** (mig `0502_task_complete_exposed.sql`):
   append idempotente di `task_complete` a `agent.tools.discovery_first_whitelist`,
   `agent.tools.core_whitelist`, `agent.tools.inline_core_whitelist`,
   `automation.o_series_essential_tools`, `automation.study_mode_readonly_tools`
   (dichiarare l'esito e' read-only). Senza, i filtri del catalogo lo strippavano.

3. **Consumo strutturato dell'esito** (motore nativo):
   - `NativeRunOutcome.declared_outcome` propaga il dict normalizzato dallo stato
     del grafo al finalizzatore unico;
   - `native_outcome_to_run_result`: `blocked`/`needs_input` o `refusal=true` ->
     `BlockedNeedsInput`; `partial` -> `FailedDiagnosed` (mai "completed" su
     lavoro dichiarato incompleto); il declared ha precedenza sul forced_close
     anti-loop (piu' specifico); il `summary` fa da `final_answer` se il modello
     ha chiuso senza testo;
   - routing gate G1: `partial` si aggiunge a done/blocked/needs_input tra le
     dichiarazioni che ESCLUDONO il reroute (dichiarazione onesta, rimandare il
     modello contro la sua stessa dichiarazione produrrebbe il loop).

4. **Turno dichiarativo forzato** (executor, `forced_declaration_delta`): prima
   di una chiusura DI SISTEMA (abort anti-loop a 0 file toccati, cap G1 a catena
   esaurita), UNA TANTUM per run, il modello riceve un turno col catalogo ridotto
   a SOLO `task_complete` e tool choice forzata: l'esito del run diventa la SUA
   dichiarazione strutturata invece di un testo sintetico. Al rientro con
   `declared_outcome` presente la testa dell'executor chiude d'autorita' col
   summary (nessun turno LLM aggiuntivo). Se il modello non dichiara nemmeno
   sotto forcing, la chiusura successiva e' quella secca storica (bounded).
   Flag di stato in `extra`: `force_outcome_declaration` (finestra corrente,
   consumato dal turno) e `outcome_declaration_forced` (una tantum per run).

Principi (allineati RFC 9457 + best practice structured output):
- `outcome`/`blocker`/`refusal` sono ENUM/bool = segnali macchina; `summary`/
  `blocked_by` sono testo umano solo per display, MAI per decidere (come
  `type`/`code` vs `detail`).
- Enforcement dello schema: la validazione e' client-side nel punto unico
  `normalize_declared_outcome` (outcome fuori enum -> dichiarazione ignorata;
  `blocker` fuori enum -> campo scartato, il resto valido resta). Non si usa
  `response_format: json_schema` (incompatibile con le tool call su quasi tutti
  i provider): il vincolo e' il TOOL, compatibile col loop agentico esistente.
  Lo schema non usa `additionalProperties`/`title` -> nessun indebolimento da
  `clean_schema_for_google`.
- SELF-REPORT != GROUND TRUTH: l'esito dichiarato va SEMPRE verificato
  oggettivamente dal `final_gate` (build/test reali), che resta l'autorita';
  `files_touched` e' telemetria (il ground truth resta
  `modified_files_from_messages`).

## Conseguenze

- La lettura dell'INTENZIONE del modello e' deterministica (campo macchina);
  le euristiche lessicali (`detect_unfulfilled_intent`, pending-steps) restano
  SOLO come fallback per i run in cui il modello non dichiara spontaneamente e
  non e' scattata una chiusura di sistema (difesa in profondita', mig 0422).
- I run che prima chiudevano "completed" con un testo diagnostico di sistema
  (incidente run c4fa064b) ora chiudono con l'esito dichiarato dal modello
  (`BlockedNeedsInput`/`FailedDiagnosed` + summary umano).
- Il `closure_judge` (mig 0391/0422) resta una via complementare NON portata al
  nativo: con la dichiarazione strutturata sempre disponibile il suo valore
  residuo e' ridotto; decidere in un ADR futuro se portarlo o ritirarlo.
- Il bridge legacy (`brain_agent_client.rs`, `resigned_patterns`) non e' stato
  toccato: e' un ponte opzionale fuori dal path primario.

## Riferimenti

- ADR 0033 (classificazione errori deterministica, strict pin, anti-loop onesto)
- Mig 0391/0422 (closure_judge, catena dei segnali di chiusura), mig 0502
  (esposizione whitelist)
- RFC 9457 Problem Details; OpenAI Structured Outputs (constrained decoding);
  pattern Pydantic/Instructor (validate-at-boundary + repair-retry + cap)
- Regole CLAUDE.md: G (fonte unica), H (fix definitivo), L (punto unico),
  M (segnali strutturati, mai parsing del testo)
