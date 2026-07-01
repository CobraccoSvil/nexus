# ADR 0034 - Esito conversazione strutturato (tool finish_task)

Stato: Proposto (design; NON ancora implementato)
Data: 2026-07-01

## Contesto

Con ADR 0033 la classificazione dell'errore PROVIDER/trasporto e' deterministica
(status HTTP + codice strutturato, mai la prosa). Resta un secondo punto che
usa ancora euristiche testuali: la determinazione dell'ESITO della conversazione
col modello (il "il modello non riesce"). Oggi si rileva via pattern testuali
(`resigned_patterns`) e la chiusura onesta read-only introdotta in ADR 0033: e'
fragile per lingua/parafrasi, esattamente come lo era la classificazione errori.

Ricerca (RFC 9457 Problem Details; structured outputs / constrained decoding;
pattern Pydantic/Instructor) indica la via deterministica: far dichiarare al
modello il proprio esito in un OUTPUT STRUTTURATO garantito da schema.

## Decisione (proposta)

Introdurre un tool `finish_task` a schema STRICT che l'agente chiama per chiudere
il turno, invece di produrre prosa:

```
finish_task(
  outcome:  enum["completato","bloccato","parziale"],   // codice macchina
  blocker?: enum["dipendenza","credenziale","permesso","servizio","ambiguita_richiesta"],
  refusal?: boolean,                                     // rifiuto safety, non prosa
  files_touched: string[],
  summary: string                                        // testo umano, solo display
)
```

Principi (allineati RFC 9457 + best practice structured output):
- `outcome`/`blocker`/`refusal` sono ENUM/bool = segnali macchina; `summary` e'
  testo umano solo per display, MAI per decidere (come `type`/`code` vs `detail`).
- Schema garantito dove supportato: OpenAI/Gemini via schema nativo, Anthropic via
  strict tool use (input_schema); Mistral/DeepSeek garantiscono solo la forma JSON
  -> validazione client-side + retry di riparazione (feedback del ValidationError
  specifico al modello, cap sui tentativi, poi fallback deterministico).
- Vincolo tool-calling: su quasi tutti i provider `response_format: json_schema`
  NON coesiste con le tool call -> si usa il TOOL `finish_task`, non il response
  format, cosi' resta compatibile col loop agentico esistente.
- SELF-REPORT != GROUND TRUTH: l'esito dichiarato va SEMPRE verificato
  oggettivamente dal `final_gate` (build/test reali). Il campo rende deterministica
  la LETTURA dell'intenzione del modello, non la verita' dell'esito.

## Conseguenze

- Sostituisce l'ultimo punto a euristica testuale (`resigned_patterns`) con un
  campo macchina.
- Il `final_gate` resta l'autorita' sull'esito reale (nessuna regressione:
  self-report + verifica).
- Attenzione a `clean_schema_for_google` (ADR-nota): strippa
  `additionalProperties`/`title`; lo schema di `finish_task` va verificato/whitelistato
  li' o quei campi preservati, altrimenti lo schema strict si indebolisce su Google.

## Perche' non implementato in questo giro

E' un cambiamento cross-cutting del loop agentico (tool catalog + executor +
end_turn + final_gate + gestione per-provider dei provider json-shape-only). Va
implementato deliberatamente con i suoi test/golden, non innestato in un turno di
chiusura (regola H: niente toppa), tanto piu' con un processo concorrente che
edita lo stesso tree. Questo ADR ne cattura il design perche' sia pronto.

## Riferimenti

- ADR 0033 (classificazione errori deterministica, strict pin, anti-loop onesto)
- RFC 9457 Problem Details; OpenAI Structured Outputs (constrained decoding);
  pattern Pydantic/Instructor (validate-at-boundary + repair-retry + cap)
- Regole CLAUDE.md: G (fonte unica), H (fix definitivo), L (punto unico)
