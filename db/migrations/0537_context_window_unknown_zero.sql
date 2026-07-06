-- 0537_context_window_unknown_zero.sql
--
-- ROOT CAUSE (incidente sub-agenti 2026-07-06, progetto Beaty-Book):
-- ai_price_catalog.context_window aveva DEFAULT 8192 (mig 0032). Il worker
-- catalog_sync inserisce i modelli scoperti via API SENZA valorizzare la
-- finestra -> ogni modello nuovo entrava con context_window=8192 come se fosse
-- il valore REALE. Il motore nativo la usa per il predictive context cap
-- (ratio 0.8 -> cap ~6553 token): un sub-run su mistral-medium-3 (finestra
-- vera 131k, catalog 8192) superava il "cap" gia' alla 2a iterazione e da li'
-- OGNI chiamata tool veniva bloccata con errore sintetico (is_error=true).
-- Il figlio ha iterato 33 volte senza mai scrivere un file, poi ha chiuso
-- 'completed' con summary vuoto/allucinato. Stessa famiglia dell'incidente
-- DeepSeek v4 (mig 0258).
--
-- FIX DEFINITIVO (regola H, tre gambe):
--   1. (codice) catalog_sync scrive la finestra DICHIARATA dal provider nella
--      discovery quando l'API la espone (Mistral max_context_length), e la
--      riallinea a ogni tick sulle righe esistenti (self-healing);
--   2. (codice) quando il provider NON la dichiara scrive 0 = IGNOTA: i gate
--      che la usano (predictive cap, token brake, smart-upscale) si
--      disattivano su 0 per contratto documentato (native_engine
--      resolve_context_window: "0 = finestra ignota");
--   3. (questa migrazione) il DEFAULT dello schema diventa 0 e i placeholder
--      8192 mai verificati vengono azzerati. 8192 era il default dello schema:
--      nessun modello reale del catalogo ha ricevuto quel valore da una fonte
--      verificata (i valori veri arrivano dai seed espliciti di 0032/0258 o
--      da UPDATE mirati). I modelli Mistral riacquistano la finestra REALE al
--      primo tick del sync (il provider la dichiara).

ALTER TABLE ai_price_catalog
  ALTER COLUMN context_window SET DEFAULT 0;

UPDATE ai_price_catalog
   SET context_window = 0,
       updated_at = NOW()
 WHERE context_window = 8192;
