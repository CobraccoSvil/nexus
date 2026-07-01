# ADR 0033 - Strict pin con retry deterministico e governor anti-loop onesto

Stato: Accettato
Data: 2026-07-01

## Contesto

Incidente ricorrente: Nexus chiudeva i run dicendo "il modello non riesce" pur
avendo modelli capaci (Google, DeepSeek) configurati. Diagnosi su chat reali nel
DB di progetto (`beaty_book_nexus`), due modalita' di fallimento distinte, NESSUNA
imputabile ai modelli:

1. Run `gemini-2.5-flash-lite` (run 709864d2): errore
   `tutti i provider hanno fallito -> google (in cooldown, 21s rimanenti)`. Il
   provider pinnato era in un cooldown TRANSITORIO di 21s e la richiesta falliva
   subito, senza attendere ne' ritentare.
2. Run `deepseek-v4-pro` (run 80036c23): ABORT anti-loop a `read_file
   backend/server.js` (count 2), dopo che il modello aveva letto il file (388
   righe, RIUSCITO), lanciato il test e listato i file. Contesto 156K token. Il
   governor abortiva un modello che stava lavorando, con messaggio "ESITO: non
   completato / mi sono bloccato", percepito come incapacita' del modello.

Cause radici nel codice:

- Il gateway, su provider pinnato (`pin_provider`), costruiva una chain di UN
  solo provider senza retry: qualunque cooldown transitorio = hard-fail.
- La classificazione dell'errore provider era basata sul TESTO del messaggio
  (`contains("insufficient_quota")`, `contains("not enabled")`, ...), fragile per
  provider/versione/lingua.
- Il governor anti-loop trattava una rilettura idempotente (read-only) come uno
  stallo produttivo (soglia 2) e la portava fino all'ABORT con framing di
  fallimento del modello.

## Decisione

### Strict pin + retry sullo stesso modello (mai swap)

Il modello scelto (dall'utente o risolto dal routing) NON viene mai sostituito da
un altro provider. Su errore/cooldown TRANSITORIO si ritenta lo STESSO modello con
backoff esponenziale+jitter; se il provider fornisce l'header `Retry-After` (RFC
9457/7231, es. Mistral/OpenAI su 429) quello ha PRECEDENZA sul backoff calcolato
(catturato in `ProviderHttpError.retry_after_seconds`, onorato sotto il tetto
`gateway.retry.wait_short_cooldown_cap_s`); su cooldown transitorio breve si attende il residuo.
Errore solo su billing/quota, errore lato client, o retry esauriti. Punto unico:
`run_fallback` in `nexus-gateway/src/server/routes.rs`, retry policy DB-driven da
`CooldownManager::retry_policy()` (settings `gateway.retry.*`, mig 0500).

### Classificazione errori DETERMINISTICA (non testuale)

Il successo/fallimento di un provider si determina su segnali CERTI, non sulla
prosa del messaggio:

- `ProviderHttpError { status, code, message }` porta lo status HTTP numerico e il
  codice d'errore STRUTTURATO estratto dal JSON (`error.type`/`error.code`/
  `error.status`, identificatore macchina stabile). `Display` identico al vecchio
  formato, cosi' i chiamanti legacy non cambiano.
- `classify_provider_error(&anyhow::Error)` decide via `downcast` su
  `ProviderHttpError` (status+codice) e su `reqwest::Error` (predicati tipizzati
  `status()`/timeout/connessione). Nessuna classificazione sul testo. Default
  sicuro per errori ignoti: Transient (ritentare e' innocuo).
- Quirk provider senza codice strutturato (es. billing Anthropic, solo nel testo)
  sono ISOLATI nell'adapter del provider, che traduce il proprio errore in un
  codice strutturato (`anthropic_http_error`), lasciando DETERMINISTICO il punto
  di decisione generico.

### Governor anti-loop onesto e compression-aware

- Soglia dedicata piu' alta per le LETTURE idempotenti
  (`agent.repeated_action_threshold.read_only`, default 4): una rilettura non e'
  uno stallo produttivo come un build che fallisce.
- L'ABORT su read-only non chiude piu' con "ESITO: non completato / mi sono
  bloccato" (framing di incapacita' del modello) ma in modo ONESTO instradando al
  `final_gate`, che valuta l'esito reale.

## Conseguenze

- Il sintomo "tutti i provider hanno fallito -> X (cooldown 21s)" sparisce per il
  pin: si attende/ritenta lo stesso modello.
- Un 400 (colpa nostra) o un 403-per-modello non mette piu' in cooldown un
  provider sano ne' viene ritentato inutilmente.
- Un modello capace non viene piu' dichiarato "incapace" per una rilettura.
- Tutto DB-driven (regola G): `gateway.retry.*`, `agent.repeated_action_threshold
  .read_only` (mig 0500). Nessun fallback hardcoded nella business logic.

## Alternative scartate

- Fallback cross-provider automatico (swap del modello): scartato su richiesta
  esplicita dell'utente (determinismo di costo/qualita'/comportamento). Il
  meccanismo resta possibile per il path non-pin (`policy.decide`).
- Classificazione per testo con piu' pattern/lingue: scartata (regola H): fragile
  per definizione, e' una toppa.

## Riferimenti

- Migrazione `db/migrations/0500_routing_retry_loop_settings.sql`
- `crates/nexus-gateway/src/{cooldown.rs, server/routes.rs, providers/*}`
- `crates/nexus-agent-graph/src/nodes/executor.rs`,
  `crates/nexus-agent-graph/src/decisions/progress_controller.rs`
- Regole CLAUDE.md: G (fonte unica DB), H (fix definitivo), L (punto unico)
