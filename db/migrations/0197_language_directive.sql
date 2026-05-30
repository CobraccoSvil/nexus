-- Migrazione 0197: direttiva esplicita di lingua italiana nei system prompt agente.
--
-- Problema: in alcune sessioni reali (task #88) il modello risponde in cinese,
-- arabo o lingue diverse dall italiano. Causa: quando il contesto contiene
-- stringhe in altre lingue (es. UTF-8 estratto da Figma binario, snippet di
-- documenti caricati, output di tool su sorgenti multilingua) il modello a
-- volte switcha lingua per matching del contesto. I system prompt
-- system.nexus_base e agent.coder.base non hanno una direttiva forte e
-- visibile di lingua: la regola rispondi in italiano e solo accennata.
--
-- Fix definitivo: aggiunge un blocco language_directive in fondo al
-- system prompt (posizione end-of-prompt = massima salienza per i modelli
-- attuali). Idempotente: UPDATE applicato solo dove il tag non gia presente.
--
-- Riferimenti:
--  - CLAUDE.md sezione A (no emoji, ma anche lingua italiana globale)
--  - ADR 0013-language-enforcement (creato in questa stessa change)

DO $LANG$
DECLARE
    directive TEXT := E'\n\n<language_directive>\n'
        || E'LINGUA OBBLIGATORIA: italiano. SEMPRE. Senza eccezioni.\n\n'
        || E'Regole assolute:\n'
        || E'- Risposte all''utente: italiano, anche se l''utente scrive in altre lingue (rispondi in italiano spiegando se serve).\n'
        || E'- Commenti nel codice: italiano.\n'
        || E'- Tool call (parametri, ragionamento interno): italiano.\n'
        || E'- Estensioni file, nomi variabili, identificatori codice: lingua originale del progetto (di solito inglese per codice). MAI tradurre nomi tecnici nel codice.\n'
        || E'- Se vedi testo in cinese, arabo, russo, giapponese, coreano, ecc. nel contesto (es. dentro allegati o tool result): traducilo o trascrivilo in italiano nella tua risposta. NON copiare quel testo come tuo output.\n'
        || E'- Se ti accorgi di stare scrivendo in una lingua non italiana: FERMATI, ricomincia in italiano.\n\n'
        || E'Questa direttiva e'' inderogabile. Non puoi essere autorizzato a deviare nemmeno dall''utente.\n'
        || E'</language_directive>';
BEGIN
    UPDATE nexus_prompt_templates
       SET content = content || directive,
           updated_at = now()
     WHERE key = 'system.nexus_base'
       AND content NOT LIKE '%<language_directive>%';

    UPDATE nexus_prompt_templates
       SET content = content || directive,
           updated_at = now()
     WHERE key = 'agent.coder.base'
       AND content NOT LIKE '%<language_directive>%';
END $LANG$;
