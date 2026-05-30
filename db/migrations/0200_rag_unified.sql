-- Migrazione 0200: RAG strutturale unificato (ADR 0015).
--
-- - Aggiunge colonne di tracking indicizzazione su chat_message_attachments.
-- - Inserisce i settings di configurazione RAG (chunking, top_k, endpoint, on/off).
--
-- Tutto idempotente.

ALTER TABLE chat_message_attachments
  ADD COLUMN IF NOT EXISTS indexed_at TIMESTAMPTZ NULL,
  ADD COLUMN IF NOT EXISTS chunk_count INTEGER NOT NULL DEFAULT 0;

CREATE INDEX IF NOT EXISTS idx_chat_message_attachments_indexed_at
  ON chat_message_attachments (indexed_at);

INSERT INTO settings (key, value, description) VALUES
  ('agent.rag.enabled', 'true',
   'Abilita la pipeline RAG strutturale per allegati/KB/chat-history/tool-results.'),
  ('agent.rag.chunk_size', '1000',
   'Dimensione caratteri di un chunk per la pipeline RAG.'),
  ('agent.rag.chunk_overlap', '200',
   'Overlap caratteri fra chunk consecutivi per la pipeline RAG.'),
  ('agent.rag.top_k_default', '8',
   'Numero default di hit ritornati da search_semantic se top_k non specificato.'),
  ('agent.rag.embedding_endpoint', '/embed',
   'Path REST sul brain per ottenere embeddings batch. Canonico /embed (riusa EmbeddingService).'),
  ('agent.rag.qdrant_url', 'http://localhost:6333',
   'URL Qdrant per le collection RAG (attachment_chunks, kb_chunks, ecc.).'),
  ('agent.rag.embedding_dim', '384',
   'Dimensione vettori embedding (all-MiniLM-L6-v2 = 384).'),
  ('agent.rag.collection_attachments', 'attachment_chunks',
   'Nome collection Qdrant per chunks allegati.'),
  ('agent.rag.collection_kb', 'kb_chunks',
   'Nome collection Qdrant per chunks knowledge base.'),
  ('agent.rag.collection_chat_history', 'chat_history_chunks',
   'Nome collection Qdrant per chunks history chat.'),
  ('agent.rag.collection_tool_results', 'tool_results_chunks',
   'Nome collection Qdrant per chunks tool results di grandi dimensioni.')
ON CONFLICT (key) DO NOTHING;
