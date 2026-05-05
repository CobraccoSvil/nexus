-- Migrazione 0091: impostazioni runtime per il sistema di self-reflection (Fase 2)
--
-- Tutti i parametri di reflection sono gestiti dal DB e configurabili
-- dall'interfaccia admin (categoria "reflection").
-- Il codice Python legge questi valori ESCLUSIVAMENTE dal DB tramite
-- brain/agents/reflection_config.py con cache TTL 60s.
-- Nessuna variabile d'ambiente e' utilizzata come fallback.

INSERT INTO settings (key, value, category, description, is_secret) VALUES

    -- Feature flag globale: disabilita completamente il nodo reflection_node
    -- se impostato a 'false'. Utile per rollback immediato senza deploy.
    ('reflection_enabled',
     'true',
     'reflection',
     'Abilita il nodo di self-reflection post-esecuzione agente. '
     'Disabilita con "false" per rollback immediato senza rideploy.',
     FALSE),

    -- Probabilita' (0.0-1.0) che un run venga campionato per la reflection.
    -- 0.0 = mai, 1.0 = sempre. Default 0.3 (30%) per bilanciare costo e copertura.
    ('reflection_sample_rate',
     '0.3',
     'reflection',
     'Probabilita'' campionamento reflection per ogni run agente (0.0-1.0). '
     '0.3 = 30% dei run, 1.0 = tutti i run. Aumentare in ambienti di eval.',
     FALSE),

    -- Timeout massimo (secondi) per la chiamata LLM di reflection.
    -- Superato il timeout, il run procede senza reflection (nessuna regressione).
    ('reflection_timeout_s',
     '10',
     'reflection',
     'Timeout massimo in secondi per la chiamata LLM di valutazione. '
     'Se il modello non risponde entro questo limite, la reflection viene saltata.',
     FALSE),

    -- Modello LLM usato per la reflection (preferibilmente leggero per latenza).
    -- Deve essere disponibile nel provider Anthropic configurato.
    ('reflection_model',
     'claude-3-5-haiku-20241022',
     'reflection',
     'Modello LLM usato per la valutazione self-reflection. '
     'Preferire modelli veloci ed economici (es. claude-3-5-haiku-20241022).',
     FALSE),

    -- Peso del reflection_score nel calcolo del reward Q-learning finale.
    -- final_reward = (1 - peso) * heuristic + peso * reflection_score
    -- Default 0.3 (30% reflection, 70% euristica).
    ('reflection_reward_weight',
     '0.3',
     'reflection',
     'Peso del punteggio reflection nel reward Q-learning finale (0.0-1.0). '
     '0.3 significa: final_reward = 0.7 * euristica + 0.3 * reflection_score.',
     FALSE),

    -- Score minimo (0.0-1.0) per inserire un esempio nel reasoning bank.
    -- Esempi con score inferiore vengono ignorati (non arricchiscono i few-shot).
    ('reflection_reasoning_bank_min_score',
     '0.85',
     'reflection',
     'Punteggio minimo (0.0-1.0) perche'' un esempio di successo venga '
     'salvato nel reasoning bank per arricchire i few-shot futuri.',
     FALSE)

ON CONFLICT (key) DO NOTHING;
