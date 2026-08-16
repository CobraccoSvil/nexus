-- ─────────────────────────────────────────────────────────────────────────────
-- 0713 — groq: il 498 del flex tier e la tool-call fallita si DICHIARANO
--
-- Due righe per il catalogo della mig 0707 (nexus_provider_error_code). Le
-- cause `overloaded` e `malformed_request` sono gia' nel vocabolario chiuso di
-- `CausaErrore` e nel CHECK `causa_nel_vocabolario` (0707, riallargato da 0709):
-- nessuna ALTER, solo assegnazione (fornitore, valore, status) -> causa.
--
-- 1) ('groq','capacity_exceeded',498) -> overloaded
--    Il flex tier di groq non accoda: a capacita' esaurita rifiuta SUBITO con
--    lo status 498, fuori standard, che la doc groq elenca come «Flex Tier
--    Capacity Exceeded» (fail-fast: il rimedio e' ritentare a breve o ripiegare,
--    non attendere una quota). Sul 498 la tabella per status ricade gia' su
--    Transient e `overloaded` proietta sulla stessa classe: la riga non CAMBIA
--    la classe, rende il verdetto DICHIARATO (`FonteVerdetto::Dichiarata`
--    invece di `DalloStatus`) e toglie il valore dal debito — senza, ogni 498
--    farebbe nascere una riga di nexus_provider_error_code_unknown. Lo status
--    e' nella chiave perche' e' il 498 a dare al valore il significato
--    documentato: fuori da quel contesto non e' stato osservato, e una riga
--    senza status affermerebbe piu' di quanto la doc dichiara.
--
-- 2) ('groq','tool_use_failed',400) -> malformed_request
--    OSSERVATO nel registro ignoti (nexus_provider_error_code_unknown):
--    5 occorrenze il 13-14/08/2026. E' il modello che ha prodotto una tool-call
--    che groq non ha saputo eseguire: una richiesta malformata a tutti gli
--    effetti, e la causa CONSERVA is_history_related_client_error — qui la
--    sanificazione della history e' il rimedio giusto, perche' rimuove proprio
--    la generazione malformata. Il `type` che lo accompagna e'
--    `invalid_request_error`, gia' riconosciuto dal jolly della 0707 con la
--    stessa causa: la riga sul `code` non cambia il verdetto, chiude il debito.
-- ─────────────────────────────────────────────────────────────────────────────

INSERT INTO nexus_provider_error_code
  (provider, valore, http_status, causa, campo, origine, occorrenze_al_seed, nota) VALUES
('groq','capacity_exceeded',498,'overloaded','/error/code','doc',NULL,
 'flex tier fail-fast: a capacita'' esaurita groq rifiuta subito con 498 invece di accodare. Stessa classe che lo status gia'' dava (Transient): la riga rende il verdetto dichiarato e chiude il debito'),
('groq','tool_use_failed',400,'malformed_request','/error/code','measured',5,
 'il modello ha prodotto una tool-call che groq non ha saputo eseguire; accompagnato da type=invalid_request_error. Conserva is_history_related_client_error: sanificare la history rimuove la generazione malformata')
ON CONFLICT DO NOTHING;
