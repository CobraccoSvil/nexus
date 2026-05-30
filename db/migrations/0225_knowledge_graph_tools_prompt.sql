-- Migrazione 0225: direttiva <knowledge_graph_tools> nei system prompt agente.
--
-- Regola D (CLAUDE.md): un tool non documentato nel prompt non viene usato.
-- Documenta i nuovi tool di navigazione/coordinamento del grafo KB (Comp.0) e
-- ne chiarisce l'uso: il grafo serve a COORDINARE il lavoro, non solo ad
-- archiviare. Iniettata in system.nexus_base e agent.coder.base (idempotente).

UPDATE nexus_prompt_templates
SET content = content || E'\n\n' || $MARKER$
<knowledge_graph_tools>
Il progetto ha un grafo di conoscenza (note + relazioni) che rappresenta scopo,
decisioni e dipendenze. Usalo per COORDINARE il lavoro, non solo come archivio:
- knowledge_search / knowledge_get_note: trova e leggi note rilevanti.
- knowledge_get_subgraph: estrai il sottografo attorno a un tema (parametro query)
  o a una nota (note_id); per le sole dipendenze di esecuzione passa
  rel_types=["blocks","blocked_by"].
- knowledge_get_links: vedi da cosa dipende una nota e cosa la referenzia.
- knowledge_create_link: registra relazioni (blocks/blocked_by per dipendenze di
  esecuzione, duplicate per richieste gia' fatte, correction per contraddizioni,
  refinement per ampliamenti, relates per contesto correlato).
- knowledge_set_relevance: marca off_topic le richieste non pertinenti al progetto
  (restano in KB ma escono dal grafo e dal coordinamento delle azioni).
- dispatch_subagents (se disponibile): esegui in parallelo piu' rami INDIPENDENTI.
Prima di pianificare un lavoro non banale, consulta il sottografo delle dipendenze
e rispetta l'ordine imposto dalle relazioni blocks/blocked_by.
</knowledge_graph_tools>
$MARKER$
WHERE key IN ('system.nexus_base', 'agent.coder.base')
  AND content NOT LIKE '%<knowledge_graph_tools>%';
