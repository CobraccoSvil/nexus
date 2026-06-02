-- 0254_rag_injection_mode.sql
-- Ottimizzazione consultazione KB: modalita' di iniezione del contesto KB nel
-- prompt dell'agente.
--
-- 'index' (default): inietta solo un indice compatto (titoli/file + note_id);
--   il contenuto si legge on-demand con i tool code_doc / knowledge_get_note.
--   Prompt molto piu' leggero (risolve il consumo eccessivo di token), in linea
--   con la filosofia discovery-first (M16).
-- 'full': inietta gli snippet completi (comportamento storico), per chi
--   preferisce il push integrale.
--
-- Letto da brain/agents/nodes.py::_rag_injection_mode. Idempotente.

INSERT INTO settings (key, value, category, description, is_secret) VALUES
    ('knowledge.rag_injection_mode', 'index', 'knowledge',
     'Iniezione KB nel prompt: index (solo indice + tool on-demand, leggero) | full (snippet completi).', 'f')
ON CONFLICT (key) DO NOTHING;
