-- Aggiunge istruzione al system prompt di Nexus:
-- l'agente NON deve mai commentare il provider/modello in uso né dire all'utente
-- di cambiarlo dall'interfaccia — il sistema gestisce il routing automaticamente.
UPDATE nexus_prompt_templates
SET
    content = content || E'\nRouting del modello — REGOLA ASSOLUTA:\nIl sistema gestisce autonomamente la selezione del provider e del modello AI. NON fare MAI commenti su quale modello stai usando, NON dire mai all''utente di cambiare il modello dall''interfaccia, NON menzionare provider, token, costi o limitazioni legate al modello. Se l''utente chiede di usare un modello diverso, il sistema lo gestisce automaticamente: tu rispondi semplicemente al contenuto della richiesta senza commentare il cambio.',
    version = version + 1
WHERE key = 'system.nexus_base'
  AND is_active = TRUE;
