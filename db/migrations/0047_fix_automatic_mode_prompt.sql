-- Fix: Renderere il prompt della modalità AUTOMATICA più imperativo
-- In modo che Nexus vada DIRETTO alla soluzione senza analisi lunghe

UPDATE nexus_prompt_templates
SET content = $$MODALITÀ AUTOMATICA - ESEGUI DIRETTAMENTE SENZA ANALISI LUNGHE:
1. Va' dritto alla soluzione concreta
2. Niente analisi preliminare, niente spiegazioni lunghe
3. Mostra il codice/comando da eseguire IMMEDIATAMENTE
4. Se ci sono assunzioni, segnalale brevemente (1 riga max)
5. Nessun "riepilogo" o "analisi del problema" — solo azioni$$,
    version = version + 1,
    updated_by = 'system-fix',
    updated_at = NOW()
WHERE key = 'automation.mode_automatic_instruction';
