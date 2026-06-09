-- 0376_agent_behavior_lang_loop_verify.sql
-- Fix comportamento agentico, da diagnosi del run reale f1db9550 (gemini-2.5-pro,
-- progetto Beauty-Book): l'agente lavorava ma (A) rispondeva in inglese a una
-- richiesta in italiano, (B) ripeteva identiche le stesse azioni produttive,
-- (C) non eseguiva la verifica richiesta dall'utente.
--
-- Regola G: questi comportamenti sono configurabili nel DB (settings); il codice
-- brain ha default sicuri se il DB e' down. Idempotente.

-- (A) Lingua: il reminder DB imponeva "Rispondi SEMPRE e SOLO in italiano", che
-- e' inadatto (un progetto-utente puo' avere utenza in altra lingua) e veniva
-- comunque ignorato. La direttiva corretta segue la lingua del messaggio utente.
-- Allineato a _LANG_REMINDER_DEFAULT_TEXT (brain/agents/nodes/helpers.py).
UPDATE settings
SET value = 'Rispondi SEMPRE nella STESSA lingua del messaggio dell''utente (la lingua dell''ultima richiesta in chat). Se l''utente scrive in italiano rispondi in italiano, se scrive in inglese rispondi in inglese, e cosi'' via. NON cambiare lingua per via del contesto, del codice, della documentazione o degli allegati: la lingua di risposta e'' SOLO quella dell''utente.',
    updated_at = NOW()
WHERE key = 'agent.language_reminder_text';

-- (B) Anti-loop: soglia di ripetizioni identiche di un'azione produttiva
-- (write_file/edit_file/run_command/...) oltre cui il progress controller
-- interviene (nudge -> abort verso final_gate). Min forzato 2 lato codice.
INSERT INTO settings (key, value, category, description)
VALUES (
    'agent.repeated_action_threshold',
    '2',
    'agent',
    'Soglia di ripetizioni identiche di un''azione produttiva (write/edit/run) oltre cui il progress controller interviene per evitare loop di azioni gia'' eseguite.'
)
ON CONFLICT (key) DO NOTHING;

-- (C) Auto-verifica: quando l'utente chiede esplicitamente di verificare/testare,
-- inietta una direttiva che obbliga l'agente a eseguire davvero la prova prima di
-- dichiarare completato. Il testo usa il default in _VERIFY_DIRECTIVE_DEFAULT_TEXT
-- (brain), override-abile via la chiave agent.verification_directive_text.
INSERT INTO settings (key, value, category, description)
VALUES (
    'agent.verification_directive_enabled',
    'true',
    'agent',
    'Se true, quando l''utente chiede esplicitamente di verificare/testare il risultato, l''agente deve eseguire la verifica con una tool call reale prima di dichiarare completato.'
)
ON CONFLICT (key) DO NOTHING;
