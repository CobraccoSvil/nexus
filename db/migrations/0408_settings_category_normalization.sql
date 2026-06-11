-- 0408: Normalizzazione categorie settings (audit configurazioni 2026-06-11).
--
-- La sidebar admin ora deriva le voci dai DATI (SELECT DISTINCT category,
-- endpoint /api/admin/settings-categories): ogni categoria e' navigabile.
-- Questa migrazione consolida SOLO i sinonimi/frammenti ovvi emersi
-- dall'audit (39 categorie live, molte con 1-2 chiavi), senza tassonomie
-- speculative (regola H). Idempotente: UPDATE su predicati stabili.

-- Sinonimi singolari/plurali e frammenti dello stesso dominio.
UPDATE settings SET category = 'agent'   WHERE category = 'agents';
UPDATE settings SET category = 'agent'   WHERE category = 'automation';

-- Worker di catalogo/health/promozione modelli: stesso dominio del routing
-- (routing matrix, catalog sync, health probe provider e modelli).
UPDATE settings SET category = 'routing' WHERE category IN ('ai', 'monitoring', 'router');

-- Configurazione di processo/infrastruttura.
UPDATE settings SET category = 'infrastructure' WHERE category IN ('runtime', 'system');

-- Collection e soglie vettoriali: dominio embeddings.
UPDATE settings SET category = 'embeddings' WHERE category = 'vector';

-- Chiavi agent.* finite nella categoria generica 'general' (seed storici).
UPDATE settings SET category = 'agent' WHERE category = 'general' AND key LIKE 'agent.%';

-- Residuo meta_docs: vault_path e' l'unica superstite del sistema legacy
-- (ADR 0017 v2); il vault Markdown e' oggi gestito dal wiki unificato.
UPDATE settings SET category = 'wiki' WHERE category = 'meta_docs';
