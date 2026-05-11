-- Migrazione 0123: settings configurabili per Extended Thinking (Anthropic)
--
-- Extended Thinking genera token di ragionamento interno billati al prezzo
-- output (3-15x il prezzo input). Era attivato automaticamente su ogni
-- turno Sonnet/Opus causando costi imprevisti (~$15-25/giorno in test).
-- Con questa migrazione diventa opt-in controllabile da admin senza rideploy.
--
-- Categoria 'agent': raggruppa parametri comportamentali degli agenti AI.

INSERT INTO settings (key, value, category, description, is_secret) VALUES

    ('extended_thinking_enabled',
     'false',
     'agent',
     'Abilita il ragionamento interno esteso (Extended Thinking) di Anthropic '
     'sui modelli Sonnet/Opus. Genera token di ragionamento interni billati al '
     'prezzo output. Disabilitato di default per contenere i costi. '
     'Attivare solo per task che richiedono ragionamento profondo.',
     FALSE),

    ('extended_thinking_budget_tokens',
     '8000',
     'agent',
     'Budget massimo di token interni di ragionamento per turno quando '
     'extended_thinking_enabled=true. Range consigliato: 2000-16000. '
     'Valori piu'' alti migliorano la qualita'' ma aumentano i costi.',
     FALSE)

ON CONFLICT (key) DO NOTHING;
