-- 0544: alza max_token_budget da 32000 a 262144 (256k).
--
-- Il tetto 32000 veniva dalla migrazione INIZIALE 0002_settings.sql, di quando i
-- modelli avevano ~32k di contesto. resolve_token_budget (model_routing.rs) cappa
-- il budget di token del run a questo valore, e il budget governa ANCHE il tier
-- (tier_for_tokens). Mai aggiornato mentre i modelli crescevano: strozzava OGNI
-- run agentico a 32k indipendentemente dal context window del modello (google e
-- deepseek offrono 1M, mistral 262k), bloccando i run lunghi con "N/32000" anche
-- su modelli capienti (final_answer "limite di token del contesto 30525/32000").
--
-- 262144 = context del modello disponibile piu' piccolo (mistral-medium-3): tutti
-- i modelli lo reggono senza saturare. Resta sotto il tetto cumulativo anti-runaway
-- agent.run_token_budget=400000. DB-driven (regola G): configurabile, cache
-- RoutingConfig 60s. UPDATE idempotente: nessun task piccolo ne risente (solo i run
-- che prima superavano 32000).
UPDATE settings
SET value = '262144',
    description = 'Tetto massimo del token budget per run (cappa msg_tokens_estimate e governa il tier via tier_for_tokens). Alzato da 32000 (retaggio mig 0002, era dei modelli ~32k) a 262144 per non strozzare i run agentici su modelli a context ampio.',
    updated_at = NOW()
WHERE key = 'max_token_budget';
