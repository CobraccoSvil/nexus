-- 0590_capability_locked_separato_da_source.sql
-- Fase 2 del design "gate di qualificazione modelli" (D:\IDEAI-runtime\
-- nexus-design\2026-07-14_design_gate_qualificazione_modelli.md), punto 6.
--
-- Root cause: `capability_source='manual'` cumulava DUE semantiche distinte:
-- (a) provenienza dei flag ("curati dall'admin, il catalog_sync non li
-- riscrive" — scopo originale legittimo, ADR 0024) e (b) immunita' dalla
-- verifica automatica (il guard in tool_capability::record_tool_failure
-- saltava le righe manual: nessun contatore, nessun degrado, MAI). La (b)
-- rendeva le dichiarazioni manuali INFALSIFICABILI proprio nel caso in cui la
-- verifica serve di piu' (incidente glm-4.7-flash: capabilities dichiarate a
-- mano in migrazione, mai provate, immuni al degrado runtime).
--
-- Fix: `capability_locked` come colonna DEDICATA del lock. Il backfill 1:1
-- (= capability_source='manual') NON cambia alcun comportamento oggi: le
-- righe curate restano protette esattamente come prima (incidente deepseek-v4
-- 2026-06-10 coperto). La differenza e' di governo: il lock diventa una
-- scelta esplicita e revocabile riga per riga, separata dalla provenienza —
-- l'admin puo' dichiarare flag a mano E lasciarli falsificabili dal runtime.
--
-- Idempotente: ADD COLUMN IF NOT EXISTS; il backfill riallinea solo le righe
-- non ancora lockate con source manual.

ALTER TABLE ai_price_catalog
  ADD COLUMN IF NOT EXISTS capability_locked BOOLEAN NOT NULL DEFAULT false;

UPDATE ai_price_catalog
   SET capability_locked = true
 WHERE capability_source = 'manual'
   AND capability_locked = false;

COMMENT ON COLUMN ai_price_catalog.capability_locked IS
  'Lock esplicito della curatela: true = i writer automatici del ciclo tool-capability (record_tool_failure, degrado a soglia) non toccano la riga. Separato da capability_source (provenienza dei flag): mig 0590.';
