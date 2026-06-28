-- 0473_billing_cooldown_logico.sql
--
-- ROOT CAUSE (regola H + L): il billing/credito esaurito di un provider veniva
-- PERSISTITO come disabilitazione del catalog/matrix
-- (ai_price_catalog.is_enabled=false, nexus_routing_matrix.is_active=false) da
-- propagate_billing_disable_to_db. Persistenza sbagliata: is_enabled significa
-- "il modello e' valido", non "ora senza credito". Lo stato corretto del billing
-- e' uno STATO LOGICO TRANSITORIO con TTL su nexus_provider_health.billing_cooldown_until
-- (gia' esistente, e' la persistenza giusta perche' scade). Il routing salta i
-- provider in cooldown via is_provider_in_cooldown/cooldown_snapshot senza bisogno
-- di is_enabled=false.
--
-- Questa migrazione ripristina lo stato corretto delle righe spente per billing,
-- ora che la propagazione persistente e' stata rimossa dal codice (Rust).

BEGIN;

-- Catalog: riaccendi i modelli spenti SOLO per billing (transitorio, non e' una scelta di capability).
UPDATE ai_price_catalog
SET is_enabled = true, auto_disabled_at = NULL, auto_disabled_reason = NULL, updated_at = NOW()
WHERE auto_disabled_reason LIKE 'auto_disable: billing_cooldown%';

-- Routing matrix: riattiva e pulisci il tag billing dalle notes. manual_override NON viene toccato
-- per non "spinnare" eventuali righe che erano pin legittimi (verificabile separatamente).
UPDATE nexus_routing_matrix
SET is_active = true,
    notes = NULLIF(regexp_replace(COALESCE(notes,''), ' \[auto_disable: billing_cooldown:[^\]]*\]', '', 'g'), '')
WHERE notes LIKE '%auto_disable: billing_cooldown%';

COMMIT;
