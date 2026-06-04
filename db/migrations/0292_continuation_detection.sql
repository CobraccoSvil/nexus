-- Migrazione 0292 — Continuation detection settings (ADR 0017 Fix A).
--
-- Quando un turno chiude con end_turn ma final_answer contiene una
-- promessa di proseguire ("sto procedendo", "I'll proceed to..."), Nexus
-- rilancia automaticamente l'agente con un prompt di follow-up. Caso reale:
-- chat 6 Beauty-Book run e38aaba7 (12:51) — Gemini 2.5 Pro ha narrato
-- "Sto procedendo con la creazione di altri test" ma ha emesso end_turn.

INSERT INTO settings (key, value, category, description, updated_at) VALUES
    ('agent.continuation.auto_restart_enabled', 'true', 'agent',
     'Se true, quando un turno chiude con end_turn ma final_answer matcha pattern di continuazione (sto procedendo / I''ll proceed) senza marker di completamento, Nexus rilancia automaticamente l''agente con il follow_up_prompt. Solo se automation_mode in (automatic, continuous) e supervisor_mode in (continuous, every_step, on_anomaly). Default true.',
     NOW()),
    ('agent.continuation.max_auto_restarts', '3', 'agent',
     'Limite di auto-restart consecutivi per lo stesso run (evita loop infiniti su modelli ostinati). Default 3.',
     NOW()),
    ('agent.continuation.min_promise_recency_chars', '200', 'agent',
     'Lunghezza (caratteri) della coda del final_answer in cui cercare i pattern di continuazione. Una promessa in mezzo al testo seguita da azioni finali NON conta. Default 200.',
     NOW()),
    ('agent.continuation.follow_up_prompt',
     'Hai dichiarato di voler proseguire ma hai chiuso il turno senza farlo. Esegui ORA i prossimi passi che avevi promesso, usando i tool del progetto. Quando hai veramente finito tutti i task, scrivi come ultima riga: TASK COMPLETATO.',
     'agent',
     'Prompt iniettato come messaggio utente nel turno auto-restartato. Spiega al modello che deve agire (NON narrare) e fornisce il marker esplicito da usare per dichiarare il completamento (TASK COMPLETATO).',
     NOW())
ON CONFLICT (key) DO NOTHING;
