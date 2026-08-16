-- 0723_purpose_model_tier_only.sql (F3-03, fase 3 lotto 2)
--
-- nexus_purpose_model diventa TIER-ONLY: cadono le colonne provider/model_id.
--
-- Il runtime le ignora da tempo: resolve_purpose_core (internal_routing.rs) e'
-- tier-only — con tier valorizzato risolve dal catalog, con tier NULL ritorna
-- NotFound — e la mappa RoutingMatrix.purpose_models che le caricava non aveva
-- alcun lettore decisionale (solo un len() nel log di init). Le due colonne
-- DICEVANO un pin che nessuno usa: il pannello admin le mostrava come "il
-- modello del purpose" (misurato il 2026-07-16: figure dichiarate
-- deepseek/deepseek-v4-flash giravano su groq/gpt-oss-20b). Un pannello che
-- mostra una configurazione invece dell'effetto sembra una verifica, ed e' una
-- bugia (doc di admin/routing.rs). Drop, non azzeramento: una colonna azzerata
-- continuerebbe a raccontare che un pin statico esiste.
--
-- CURA della sola riga con tier NULL (misurata sul DB vivo il 16/08/2026, 74
-- righe totali, 1 senza tier): ui_reference_search (mig 0652) dichiarava "tier
-- NULL di proposito: serve il modello con la ricerca web, e il modo di dirlo
-- e' il model_id statico". Quel modo NON esiste piu': la risoluzione statica
-- e' sparita quando il resolve e' diventato tier-only, quindi il tool riceveva
-- 404 "purpose non configurato o privo di tier" a OGNI chiamata (il pin era
-- gia' morto). La cura e' il tier: il discriminante vero e' la capability che
-- il purpose gia' dichiara (required_capability='web_search' — nel catalog
-- vivo la portano solo i modelli sonar di perplexity). Tier 'medium' per la
-- stessa ragione della 0652 (sonar e' il piu' economico dei tre: per dedurre
-- convenzioni di interfaccia non serve il modello che ragiona); con sonar
-- (medium) disabilitato la TierPolicy::Flexible del resolver sale a sonar-pro
-- (high) invece di fallire — verificato sul catalog vivo: sonar-pro e
-- sonar-reasoning-pro abilitati, entrambi high.
--
-- La colonna tier resta NULLABLE: "purpose senza tier -> NotFound -> il
-- chiamante degrada al routing di default" e' un degrado DICHIARATO che alcuni
-- consumatori gestiscono deliberatamente (auto_remediation). A PRODURLO pero'
-- non resta nessuno: l'endpoint admin update_purpose_model pretende ora un
-- tier valido (400 su 'static'/'none'/vuoto).

BEGIN;

UPDATE nexus_purpose_model
   SET tier = 'medium',
       notes = 'Ricerca di riferimenti di interfaccia per la figura ui_ux_designer (mig 0652). Tier-only dalla mig 0723: il discriminante e'' required_capability=web_search (nel catalog la portano solo i modelli sonar), il tier e'' la fascia di costo. Il pin statico perplexity/sonar era gia'' morto: il resolve tier-only rispondeva 404 sul tier NULL.',
       updated_at = NOW()
 WHERE purpose = 'ui_reference_search' AND tier IS NULL;

-- Guardia: ogni ALTRA riga senza tier e' configurazione morta che questa
-- migrazione non ha censito — va curata qui sopra, esplicitamente, non
-- droppata alla cieca.
DO $$ BEGIN
  IF EXISTS (SELECT 1 FROM nexus_purpose_model WHERE tier IS NULL) THEN
    RAISE EXCEPTION 'nexus_purpose_model: righe senza tier (a runtime sono gia'' NotFound). Curarle assegnando il tier PRIMA del drop del pin statico, come la cura di ui_reference_search in questa migrazione.';
  END IF;
END $$;

ALTER TABLE nexus_purpose_model
    DROP COLUMN IF EXISTS provider,
    DROP COLUMN IF EXISTS model_id;

COMMENT ON TABLE nexus_purpose_model IS
'Selezione TIER-ONLY dei modelli per task interni (purpose -> tier + required_capability + requires_tool_use). Il modello concreto lo risolve resolve_purpose_core dal catalog a ogni convocazione (mig 0203, tier-only; mig 0723 rimuove il pin statico provider/model_id). Letto dal Rust con cache 60s (routing_matrix.rs) e da fetch_purpose_tier_rule_db.';

COMMIT;
