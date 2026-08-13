-- Migrazione 0710: il RAG non configura piu' il nome di una collection che
-- non scrive lui.
--
-- ROOT CAUSE. `SourceKind::Kb` leggeva `kb_chunks` e `SourceKind::MetaDoc`
-- leggeva `nexus_meta_docs`: due collection che su Qdrant NON esistono. Non e'
-- un provisioning mancante — lo scrittore di `kb_chunks` c'era davvero
-- (`knowledge_create_note` indicizzava ogni nota con
-- `index_text(SourceKind::Kb, ...)`) ed e' stato rimosso dal commit eb5e47a5
-- (knowledge graph unificato, ADR 0017 v2, 04/06/2026), che ha spostato le note
-- in `wiki_docs` + `wiki_content`. `nexus_meta_docs` e' il nome pre-unificazione
-- della stessa collection (la tabella omonima l'ha rimossa la mig 0295). Il
-- lettore e' rimasto indietro su entrambi, e nessuno se ne e' accorto perche'
-- una search su una collection inesistente non fallisce il run: produce uno
-- ZERO indistinguibile da «cercato e non trovato».
--
-- MISURATO il 13/08/2026: dieci collection su Qdrant vivo, `kb_chunks` e
-- `nexus_meta_docs` non fra queste; 6762 punti in `wiki_content` (6733 di scope
-- `project`, 29 `meta`); cinque `404 Collection kb_chunks doesn't exist` in 116
-- millisecondi a ogni run, una per figura convocata, perche'
-- `agent.subagent.mandate_recall_kinds` vale `kb,code`.
--
-- IL FIX non e' creare la collection: resterebbe vuota per sempre (nessuno
-- scrive piu' li'), e lo zero tornerebbe silenzioso — la forma in cui il difetto
-- e' gia' presente altrove, `project_docs` esiste con ZERO punti. Il nome lo
-- risolve ora il punto unico dello SCRITTORE
-- (`nexus_wiki::content_points::wiki_content_collection`, chiave
-- `agent.wiki.qdrant_collection`), come gia' accade per l'indice di codice.
--
-- Di conseguenza `agent.rag.collection_kb` non ha piu' un lettore: lasciarla
-- sarebbe una SECONDA verita' sul nome della stessa collection, che un admin
-- potrebbe cambiare credendo di spostare il RAG (regola G). Si rimuove.

DELETE FROM settings WHERE key = 'agent.rag.collection_kb';

-- La descrizione di `qdrant_url` nominava `kb_chunks` fra le collection RAG:
-- e' il nome che ha tenuto in piedi l'equivoco per due mesi.
UPDATE settings
   SET description = 'URL Qdrant per le collection SCRITTE dal RAG '
                     || '(attachment_chunks, chat_history_chunks, tool_results_chunks). '
                     || 'Le altre sorgenti interrogabili (wiki, indice di codice, '
                     || 'contesto conversazione, correzioni) hanno collection di '
                     || 'proprieta'' altrui: il nome lo risolve il loro scrittore, '
                     || 'vedi mcp-core::rag::collezioni.'
 WHERE key = 'agent.rag.qdrant_url';
