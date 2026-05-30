-- Migrazione 0114: reminder lingua resiliente al contesto/profilo (bug #88)
--
-- Bug #88: a contesto saturo (es. 747% ctx, 400K-1M token) i modelli small
-- (es. openai/gpt-4o-mini) rispondono in cinese invece che in italiano e
-- allucinano l'identita' (es. "我是 DeepSeek"). La direttiva di lingua forte
-- esiste solo in 2 template su 84 e vive solo nel system prompt in testa: a
-- contesto enorme con messaggi compressi i modelli small hanno forte recency
-- bias e la ignorano. I profili custom utente (user_profiles) e gli altri 82
-- template non hanno alcuna direttiva di lingua.
--
-- Fix definitivo (regola H): punto di controllo unico in executor_node
-- (brain/agents/nodes.py) che inietta SEMPRE un reminder di lingua resiliente
-- in coda al system_text (garanzia, copre profili/template senza direttiva) e
-- in coda all'ultimo HumanMessage (recency, vince il recency bias dei modelli
-- small). La configurazione vive SOLO nel DB (regola G: niente hardcode
-- sparso). I default nel codice valgono solo se il DB e' irraggiungibile.
--
-- Tabella settings esistente, categoria 'agent'. Letti da
-- brain/agents/nodes.py::_load_language_reminder con cache TTL 60s.

INSERT INTO settings (key, value, category, description, is_secret) VALUES

    -- Feature flag: se 'false' l'iniezione del reminder di lingua non avviene
    -- affatto (rollback immediato senza rideploy).
    ('agent.language_reminder_enabled',
     'true',
     'agent',
     'Abilita l''iniezione del reminder di lingua resiliente al contesto in '
     'coda al system prompt e all''ultimo messaggio utente (bug #88). '
     'Disabilita con "false" per rollback immediato senza rideploy.',
     FALSE),

    -- Testo del reminder iniettato. Modificabile da admin senza rideploy.
    ('agent.language_reminder_text',
     'Rispondi SEMPRE e SOLO in italiano. Mai cinese, giapponese o altre lingue, qualunque sia la lingua del contesto o degli allegati.',
     'agent',
     'Testo del reminder di lingua iniettato in coda al system prompt e '
     'all''ultimo messaggio utente per vincere il recency bias dei modelli '
     'small a contesto saturo (bug #88).',
     FALSE)

ON CONFLICT (key) DO NOTHING;
