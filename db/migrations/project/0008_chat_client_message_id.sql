-- 0008_chat_client_message_id.sql
-- Idempotenza dell'invio chat: il client genera un clientMessageId (UUID) per
-- ogni POST /messages e lo ritenta in caso di errore di rete/timeout. La colonna
-- + l'indice unico parziale garantiscono che un retry della stessa POST non
-- duplichi il messaggio utente ne' avvii un secondo agent run: il backend
-- rileva il duplicato (pre-check o unique violation 23505, segnale strutturato
-- regola M) e fa replay della risposta gia' prodotta.
--
-- Root cause del bug "invio perso su riconnessione SSE": senza chiave di
-- idempotenza il client non poteva ritentare una POST fallita in sicurezza,
-- quindi l'invio era one-shot ottimistico e un errore di rete lo perdeva.

ALTER TABLE public.chat_messages
  ADD COLUMN IF NOT EXISTS client_message_id uuid;

CREATE UNIQUE INDEX IF NOT EXISTS uq_chat_messages_session_client_message_id
  ON public.chat_messages (session_id, client_message_id)
  WHERE client_message_id IS NOT NULL;
