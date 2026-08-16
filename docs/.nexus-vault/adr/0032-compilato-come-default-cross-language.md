# ADR 0032 — Il compilato e' l'autoritativo: logica condivisa Python/Rust

Data: 2026-06-11. Stato: accettato, applicato.

## Contesto

Nexus ha due runtime che parlano agli LLM: il **brain Python** (LangGraph,
classificatore, provider) e i **crates Rust** (mcp-core orchestratore,
gateway). Diverse logiche di dominio sono nate duplicate nei due linguaggi
perche' entrambi i lati ne avevano bisogno. jscpd NON rileva i cloni
cross-language: serve un censimento semantico (fatto il 2026-06-11, 14
sovrapposizioni mappate).

## Decisione

Quando la STESSA logica vive in Python e in Rust, l'autoritativo e' il
**codice compilato** (Rust). Quattro classi, ognuna con un trattamento:

### (a) Business logic duplicata → Rust autoritativo, Python delega

La decisione vive in Rust; il Python invia dati grezzi e delega via uno dei
~10 endpoint interni `POST /api/internal/*` (canale gia' esistente).

- **Classificazione errori provider** (questa sessione): la classe
  billing/rate_limit/altro era calcolata con keyword sia in
  `brain/router/agentic_classifier.py` sia, autoritativamente, in
  `crates/mcp-core/src/provider_error_classifier.rs`. Ora `cooldown_bridge`
  invia il testo **raw** (`error_text`+`status`) a `/api/internal/provider-error`
  e mcp-core classifica col punto unico; i `_BILLING_PATTERNS`/
  `_RATE_LIMIT_PATTERNS` Python sono stati rimossi. L'endpoint resta
  retrocompatibile: un chiamante che conosce gia' la classe per le proprie
  decisioni locali (es. `registry.py` billing) puo' ancora passare
  `error_class`.
  **Aggiornamento 2026-08-13**: l'endpoint e' stato rimosso col porting
  zero-Python (nessun client vivo). L'esempio resta valido come forma — dati
  grezzi al punto unico, decisione in Rust — ma qui il punto unico si e'
  spostato ancora piu' vicino alla fonte: classifica il GATEWAY, che i segnali
  strutturati del fornitore li ha di prima mano (`tassonomia_errori`, mig
  0707), e mcp-core riceve il verdetto gia' fatto sul wire
  (`EsclusioneDichiarata`).

### (b) Accessor sottile su un punto unico nel DB → paritetico legittimo

Quando il "duplicato" e' solo un thin accessor su una tabella/vista DB, la
fonte di verita' e' il **DB**, non il linguaggio: le due implementazioni
restano e sono corrette (regola G). Non si consolidano, si documentano.
Casi: `get_setting` (`nexus-auth` ↔ `settings_db.py`), `TtlCache`
(`nexus-cache` ↔ `ttl_cache.py`), prompt registry, capability modello
(vista `0318`), cooldown **reader** (Rust e' il writer, Python legge via REST).

### (c) Logica che DEVE girare in entrambi per localita'/latenza → golden fixture

Alcune logiche pure devono essere disponibili in-process da entrambi i lati
(un round-trip sarebbe assurdo). Per queste la parita' e' garantita da una
**golden fixture condivisa** letta da pytest E da cargo test: un drift e' un
test rosso.

- `chunk_text` — `tests/fixtures/chunker_golden.json` (Wave 8a precedente)
- error classifier testuale — `tests/fixtures/error_classifier_golden.json`
- **`extract_json_block`** (questa sessione): `brain/utils/json_extract.py`
  (brace-matching con escape-tracking) e `crates/mcp-core/src/llm_json.rs`
  divergevano sugli edge case; il Rust e' stato allineato alla strategia
  robusta del Python e ora `tests/fixtures/json_extract_golden.json` (15 casi)
  e' letta da `brain/tests/test_json_extract_parity.py` e dal test Rust
  corrispondente.

### (d) Morta in entrambi → eliminata in coppia

Es. `_detect_action_request` (Python) + `detect_action_request` (Rust),
rimossi insieme nella bonifica dead-code (Wave 3, 2026-06-11).

## Pre-analisi rimandate (decisione di design aperta)

Due sovrapposizioni di classe (a) hanno un percorso runtime critico e
restano come follow-up dopo analisi dedicata:

- **6.1 usage tokens** (`registry.py::extract_usage_tokens` ↔
  `billing.rs`): normalizzazione `input_tokens`/`prompt_tokens` doppia. Far
  transitare l'usage raw e normalizzare solo in Rust tocca lo stream SSE live
  dei token (commit `fb13df1`/`0079424`) — da fare con un test E2E del
  contatore live a protezione.
- **6.3 doppia classificazione intent** (`AgenticIntentClassifier` ↔
  `orchestrator/intent.rs`): il classificatore brain produce anche metadati
  (`action_oriented` mig 0387, `report_only`) consumati dal `router_node`;
  spostare tutto in Rust richiede prima un contratto esplicito di quei
  metadati nel payload di `/api/internal/routing/decide` (dove l'`intent_hint`
  gia' viaggia, commit `4f1c99d`).

## Enforcement

`scripts/check-single-source.sh` vieta nuove implementazioni Python di
logiche catalogate Rust-only. Le golden fixture di classe (c) sono il gate di
parita'. Catalogo completo dei punti unici in ADR 0026.
