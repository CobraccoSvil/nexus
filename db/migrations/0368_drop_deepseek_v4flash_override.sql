-- 0368_drop_deepseek_v4flash_override.sql
-- Root cause del "deepseek-v4-flash hollow completion -> fallback a gemini".
--
-- deepseek-v4-flash NON e' candidato ai run agentici (supports_tool_use=false,
-- escluso da select_agentic_model). Veniva pero' scelto come modello deepseek a
-- causa di un OVERRIDE LEGACY in settings: `provider_model_deepseek =
-- 'deepseek-v4-flash'`, letto da model_routing.rs (mappa provider_models, chiave
-- `provider_model_<provider>`). Questo override scavalcava il default corretto
-- gia' presente in nexus_provider_default_model (deepseek -> deepseek-v4-pro,
-- regola G / mig 0101), violando la regola L (due fonti per la stessa decisione).
-- Usato senza tool, v4-flash gira in thinking mode e produce contenuto sotto
-- soglia (hollow completion) -> soft-failure -> fallback a gemini-2.5-pro.
--
-- Fix (regola G + L): si elimina l'override magico, lasciando UNA sola fonte di
-- verita' per il modello di default deepseek (nexus_provider_default_model =
-- deepseek-v4-pro, dual-mode tool-capable). Niente UPDATE che mantenga la doppia
-- fonte: la chiave legacy va rimossa.
-- Idempotente.

DELETE FROM settings WHERE key = 'provider_model_deepseek';

-- Igiene: i purpose worker_code_* sono tier-only (resolve_purpose_model ignora
-- il model_id statico e usa best_model_for_tier). Il model_id 'deepseek-v4-flash'
-- li' era un dato morto e fuorviante. Lo si allinea al default deepseek corretto
-- per evitare confusione in lettura (resta comunque ignorato a runtime).
UPDATE nexus_purpose_model
   SET model_id = 'deepseek-v4-pro'
 WHERE purpose IN ('worker_code_rust', 'worker_code_python',
                   'worker_code_frontend', 'worker_code_test')
   AND model_id = 'deepseek-v4-flash';
