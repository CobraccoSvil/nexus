-- provider_budget_status.notes: una cella, non un registro di eventi.
--
-- CAUSA. `deepseek_balance_sync` persisteva il balance osservato accodando la
-- propria nota (`notes = COALESCE(notes,'') || ' [sync deepseek api: ...]'`) a
-- ogni giro del worker: default 15 minuti, minimo 60 secondi, senza alcun
-- limite di lunghezza e senza un lettore — la `BudgetRow` dell'endpoint admin
-- non seleziona `notes`, e nel codice non esiste altra lettura di quella
-- colonna. Il codice e' corretto dal commit che accompagna questa migrazione
-- (sostituisce invece di accodare); qui si ripulisce cio' che ha gia' scritto.
--
-- MISURATO il 10/08/2026 sul DB meta vivo, prima di questa migrazione:
--   provider | length(notes) | occorrenze di '[sync deepseek api'
--   deepseek |        182499 |                               5363
-- e deepseek era l'UNICO provider con `notes` non NULL: gli altri sette
-- (anthropic, google, groq, kimi, mistral, openai, openrouter) hanno NULL,
-- perche' nessun altro percorso scrive questa colonna.
--
-- COSA NON SI RECUPERA, e perche' va bene: la successione dei balance passati.
-- Non era comunque uno storico utilizzabile — nessun timestamp accanto ai
-- valori, nessun ordine garantito oltre alla concatenazione, nessun lettore.
-- Il valore corrente resta nelle colonne (`spent_current_period_usd`,
-- `updated_at`), che sono i campi su cui il pannello e l'enforcement decidono.

UPDATE provider_budget_status
   SET notes = NULLIF(
         regexp_replace(notes, '(\s*\[sync deepseek api: balance=[^\]]*\])+', '', 'g'),
         ''
       )
 WHERE notes LIKE '%[sync deepseek api:%';
