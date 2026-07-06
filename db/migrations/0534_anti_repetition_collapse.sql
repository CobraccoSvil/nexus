-- 0534_anti_repetition_collapse.sql
-- Rilevamento del repetition-collapse del TESTO nel motore agentico nativo
-- (crates/nexus-agent-graph, nodo executor + decisions::text_repetition).
-- Complementare al signature-loop (agent.loop.*), che guarda la FIRMA delle TOOL
-- CALL ripetute: qui il segnale e' la periodicita' STRUTTURALE del TESTO di un
-- turno assistant.
--
-- Contesto (incidente run de7477e9, progetto Beaty-Book): il modello
-- mistral/codestral-latest, invece di eseguire la build, ha ALLUCINATO l'output
-- di un errore e' collassato ripetendo "error Command failed with exit code 1. "
-- ~898 volte (~36k caratteri). Una sola tool call nel turno -> il signature-loop
-- NON scatta; il budget token (mig 0520) non scatta (85k < 400k). Il run chiudeva
-- "completed" con quel muro di testo come final_answer.
--
-- Al rilevamento l'executor chiude il run come forced_close_unverified (via
-- close_runaway) con un recap ONESTO: mai "completed" su un output degenere non
-- verificato. Il modello che collassa viene anche penalizzato nella telemetria di
-- salute (run_outcome_blames_model su FailedDiagnosed), alimentando il failover.
--
-- Regola G: le soglie vivono nel DB, niente fallback hardcoded. I Default del
-- RepetitionThresholds valgono SOLO a DB irraggiungibile; il wiring
-- load_executor_config (native_engine.rs) passa i valori reali. Regola M: il
-- segnale e' STRUTTURALE (periodo minimo della coda via KMP failure function),
-- mai la semantica del testo. Cache 60s lato Rust (get_setting).
--
-- Retro-compatibilita': impostando agent.anti_repetition.scan_tail_cap a '0' il
-- rilevamento e' DISABILITATO -> comportamento bit-identico a prima.

INSERT INTO settings (key, value, category, description) VALUES
  ('agent.anti_repetition.min_unit_len', '1', 'agent',
   'Lunghezza minima (caratteri) dell''unita'' ripetuta considerata nel rilevamento repetition-collapse del testo di un turno assistant. 1 cattura anche il collasso di un singolo carattere ripetuto; le unita'' di soli whitespace sono comunque escluse (padding). Cache 60s (mig 0534).'),
  ('agent.anti_repetition.max_unit_len', '512', 'agent',
   'Lunghezza massima (caratteri) dell''unita'' ripetuta: oltre questa un periodo lungo e'' quasi sempre struttura legittima (paragrafi simili), non un loop degenere. Cache 60s (mig 0534).'),
  ('agent.anti_repetition.min_repeats', '20', 'agent',
   'Numero minimo di ripetizioni consecutive della stessa sottostringa oltre cui (>=) il testo del turno e'' considerato in collasso. 0 = disabilitato. Cache 60s (mig 0534).'),
  ('agent.anti_repetition.min_total_len', '400', 'agent',
   'Caratteri minimi coperti dalla ripetizione (repeats * unit_len) perche'' conti come collasso. Evita falsi positivi su ripetizioni brevi legittime: la porzione ripetuta deve essere sostanziosa. Cache 60s (mig 0534).'),
  ('agent.anti_repetition.scan_tail_cap', '16384', 'agent',
   'Caratteri della CODA del testo ispezionati (il collasso degenera verso la fine): mantiene il costo O(coda) su final_answer lunghi. 0 = rilevamento DISABILITATO (bit-identico). Cache 60s (mig 0534).')
ON CONFLICT (key) DO NOTHING;
