-- 0187_catalog_chat_only_filter.sql
--
-- Disabilita retroattivamente i modelli non chat-compatibili gia' presenti
-- in ai_price_catalog. Il filtro in catalog_sync_loop (modulo
-- mcp_core::model_catalog_sync, funzione is_chat_compatible_model) blocca
-- gli inserimenti futuri; questa migrazione pulisce lo storico.
--
-- Idempotente: WHERE is_enabled = true; aggiungendo solo righe gia' attive.
-- Etichetta auto_disabled_reason cosi' i fix DDL futuri lo riconoscono.

UPDATE ai_price_catalog
SET is_enabled = false,
    auto_disabled_at = COALESCE(auto_disabled_at, now()),
    auto_disabled_reason = 'manual: non chat-compatible (migrazione 0187 - filter retroattivo)'
WHERE is_enabled = true
  AND (
        model ~* 'voxtral'
        OR model ~* '-(tts|transcribe|realtime)-'
        OR model ~* '^tts-'
        OR model ~* '^whisper'
        OR model ~* '^dall-e'
        OR model ~* '^dalle-'
        OR model ~* '^imagen'
        OR model ~* 'embedding'
        OR model ~* '-embed($|-)'
        OR model ~* '^text-embedding'
        OR model ~* '-instruct'
        OR model ~* '^instruct-'
        OR model ~* '^babbage-'
        OR model ~* '^davinci-00'
        OR model ~* 'moderation'
        OR model = 'gemini-3.5-flash'
        OR model ~* '^gemini-1\.0'
        OR model ~* 'unknown-provider'
        OR model ~* '-unknown-'
      );
