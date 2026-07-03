-- 0511: redazione PII contestuale nel gateway (fix radice loop email
-- Beaty-Book, chat login 2026-07-03) + segnale strutturato di redazione.
--
-- CONTESTO (regola H, fix radice; regola M, segnali strutturati).
-- La pipeline di redazione del gateway (crates/nexus-gateway/src/redaction/)
-- oscura la PII (email) nell'INPUT verso il provider: il modello vede
-- '[REDACTED:email_pii]' invece del valore, lo usa letteralmente come parametro
-- SQL (zero match), conclude "e' un placeholder", ri-chiede l'email, l'utente la
-- riscrive -> viene ri-oscurata -> LOOP INFINITO. La gamba SQL (placeholder
-- usato come param nei tool_input/.env) e' gia' coperta da
-- crates/mcp-core/src/security/redaction_guard.rs (mig 0509): questo intervento
-- chiude il residuo nel LAYER DI REDAZIONE-IN-INPUT.
--
-- MOSSA (a) POLICY CONTESTUALE (opt-in, default = comportamento storico).
-- La redazione PII serve a non-leakare dati di TERZI verso log/provider;
-- l'email che l'utente scrive nel PROPRIO messaggio (role=user) come soggetto
-- del task NON e' un leak: e' input necessario. Con il flag ON la pipeline
-- applica una redazione ASIMMETRICA: NON redige le entita' PII nei messaggi
-- role=user, MA continua a redigere le PII di ogni altro ruolo (assistant/tool:
-- potenziali dati di terzi da tool_result) e i SEGRETI (API key/token/
-- connection string) su OGNI ruolo, incluso user. Default 'false': con flag OFF
-- il comportamento e' bit-identico a prima (nessun rilassamento).
--
-- MOSSA (b) SEGNALE STRUTTURATO (alla FONTE, gateway).
-- Quando una redazione viene comunque applicata, la pipeline emette un flag
-- strutturato (RedactionResult.redactions con {kind, class} + i booleani
-- RedactionStats.secret_redacted / pii_redacted), NON solo il placeholder
-- testuale. Il consumatore a valle (mcp-core routing/signals.rs::
-- tool_result_outcome_after / costruzione dello StallContext) legge il flag
-- codificato invece di fare contains("[REDACTED:") sul testo (regola M). Il
-- consumo a valle e' materia di un altro blocco: qui si espone solo il segnale.
--
-- Nessuna DDL: il segnale strutturato vive in memoria nel ciclo di una request
-- (non e' persistito qui). Questa migrazione semina il solo flag di policy.

INSERT INTO settings (key, value, category, description) VALUES
  ('gateway.redaction.skip_pii_in_user_messages', 'false', 'gateway',
   'Redazione PII asimmetrica (opt-in). ON = le entita'' PII nei messaggi role=user (dato volontario dell''utente, soggetto del task: es. email da debuggare) NON vengono oscurate verso il provider; PII di altri ruoli e SEGRETI restano sempre redatti. OFF (default) = comportamento storico (ogni PII redatta). Chiude il loop email Beaty-Book. Letto da nexus-gateway run_complete via nexus_auth::get_bool_setting (default sicuro false se DB down).')
ON CONFLICT (key) DO NOTHING;
