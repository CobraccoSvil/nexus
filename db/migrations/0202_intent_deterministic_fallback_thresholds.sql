-- 0202: soglie per il classificatore deterministico di fallback intent
--
-- Regola G di CLAUDE.md: niente costanti hardcoded nel codice per parametri
-- di routing. Queste chiavi sono la fonte autoritativa per le 2 soglie usate
-- da `deterministic_intent_fallback` in crates/mcp-core/src/orchestrator.rs.
--
-- Razionale (regola H, fix definitivo): la classificazione intent NON deve
-- dipendere esclusivamente da un LLM che puo' fallire (overload/quota/timeout).
-- Quando l'LLM classifier fallisce o degrada a `chat`, un classificatore
-- deterministico keyword/pattern entra in gioco per non perdere il path agent
-- su task chiaramente agentici (es. "Crea l'applicazione descritta nel file").
--
-- Le 2 soglie governano:
--   - intent_deterministic_high: sopra questo valore il match deterministico e'
--     cosi' confidente che si SALTA del tutto l'LLM (pre-check piu' veloce e
--     robusto). Tipicamente match agentici forti (verbo azione + contesto
--     software).
--   - intent_deterministic_min: sotto questo valore il deterministico NON viene
--     usato nemmeno come fallback quando l'LLM ricade su `chat` (evita di
--     forzare un intent su segnali troppo deboli).
--
-- Lette via cache 60s (RoutingThresholds in Rust). I default tecnici nel codice
-- (INTENT_DETERMINISTIC_HIGH_DEFAULT / _MIN_DEFAULT) sono usati solo come
-- ricovero parziale se la singola chiave manca dal DB.

BEGIN;

INSERT INTO settings (key, value, category, description) VALUES
  ('routing.intent_deterministic_high', '0.85',
   'routing',
   'Confidence del classificatore deterministico keyword sopra la quale si SALTA l''LLM (pre-check robusto per task agentici evidenti). Range [0.0, 1.0].'),
  ('routing.intent_deterministic_min', '0.60',
   'routing',
   'Confidence minima del classificatore deterministico sotto la quale NON lo si usa nemmeno come fallback quando l''LLM degrada a chat. Range [0.0, 1.0].')
ON CONFLICT (key) DO NOTHING;

COMMIT;
