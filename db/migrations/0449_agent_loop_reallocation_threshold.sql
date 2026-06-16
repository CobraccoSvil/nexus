-- 0449_agent_loop_reallocation_threshold.sql
-- Regola G: ogni setting letto dal codice deve esistere nel DB come unica fonte
-- di verita'. La soglia del loop di riallocazione porte
-- (agent.loop.resource_reallocation_threshold) era letta da
-- brain/agents/nodes/helpers.py (_load_reallocation_threshold, get_int_setting)
-- ma nessuna migrazione la inseriva: l'audit settings la classificava FANTASMA
-- (letta ma non definita) e il valore viveva solo come default hardcoded di
-- emergenza per DB down.
--
-- Diagnosi confermata (loop request_port): l'agente ripete request_port
-- variando il label, cosi' signature-loop e repeated_action non scattano (il
-- primo richiede signature identica, il secondo non traccia request_port). Il
-- segnale e' STRUTTURALE: il numero di request_port ravvicinate. Questa soglia
-- governa oltre quante chiamate il progress_controller interviene.
--
-- Valore allineato a _REALLOCATION_DEFAULT_THRESHOLD (3) in helpers.py: il
-- codice mantiene quel default SOLO come rete di sicurezza se il DB e'
-- irraggiungibile (get_int_setting non solleva mai). Soglia minima 1 lato
-- codice. Gemello di agent.repeated_action_threshold (mig 0376). Idempotente.
INSERT INTO settings (key, value, category, description)
VALUES (
    'agent.loop.resource_reallocation_threshold',
    '3',
    'agent',
    'Soglia di chiamate ravvicinate a request_port (loop di riallocazione porte) oltre cui il progress controller interviene. Segnale strutturale: l''agente varia il label per eludere signature-loop e repeated_action, conteggio in _count_recent_request_port (helpers.py). Min 1 lato codice.'
)
ON CONFLICT (key) DO NOTHING;
