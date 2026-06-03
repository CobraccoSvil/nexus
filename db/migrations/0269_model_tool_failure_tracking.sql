-- 0269: tracking auto-manutenzione tool-capability dei modelli.
--
-- Root cause (auto-manutenzione routing matrix):
--   1) L'auto-promoter (routing_matrix_auto_promoter.rs) ricostruisce le righe
--      della matrix dai modelli buoni del catalog, ma NON disattivava mai le
--      righe "stale" (modello ora broken nel catalog). Restavano is_active=true.
--      Fix: cleanup pass dentro run_one_round (codice), nessuna colonna nuova.
--   2) Modelli come gemini-2.5-pro funzionano per i task NON-tool (chat) ma
--      ritornano finish_reason=MALFORMED_FUNCTION_CALL (output vuoto) sui turni
--      con tool-forcing (task agentici). Disabilitarli globalmente (is_enabled=
--      false) li toglierebbe anche dai task chat dove funzionano. Va invece
--      marcato supports_tool_use=false, cosi' l'auto-promoter li esclude dai
--      soli intent con requires_tool_use=true, lasciandoli per chat.
--
-- Per distinguere i tool-failure dai failure generici (che portano a
-- is_enabled=false) serve un contatore DEDICATO: consecutive_tool_failures.
-- NON riusiamo consecutive_failures (semantica diversa: quello disabilita il
-- modello globalmente).

ALTER TABLE ai_price_catalog
  ADD COLUMN IF NOT EXISTS consecutive_tool_failures INTEGER NOT NULL DEFAULT 0;

COMMENT ON COLUMN ai_price_catalog.consecutive_tool_failures IS
  'Contatore di MALFORMED_FUNCTION_CALL / output-vuoto consecutivi su turni '
  'agentici con tool esposti. A soglia (settings.agent.model_tool_failure_threshold) '
  'il modello viene marcato supports_tool_use=false (non is_enabled=false), '
  'escludendolo dagli intent con requires_tool_use ma lasciandolo per chat. '
  'Reset a 0 al primo turno-con-tool andato a buon fine.';

-- Soglia tool-failure (regola G: config nel DB, niente hardcode/env).
INSERT INTO settings (key, value, category, description)
VALUES (
  'agent.model_tool_failure_threshold',
  '3',
  'routing',
  'Numero di turni agentici (con tool esposti) consecutivi chiusi con MALFORMED/output-vuoto dopo i quali un modello viene marcato supports_tool_use=false. Reset al primo successo con tool.'
)
ON CONFLICT (key) DO NOTHING;

-- Flag enforcement del cleanup pass dell'auto-promoter (default abilitato).
INSERT INTO settings (key, value, category, description)
VALUES (
  'agent.routing_matrix_cleanup_stale_enabled',
  'true',
  'routing',
  'Se true, l''auto-promoter disattiva (is_active=false) le righe della routing matrix non-manuali il cui (provider, model_id) non ha piu un modello sano nel catalog (is_enabled=true AND consecutive_failures=0).'
)
ON CONFLICT (key) DO NOTHING;
