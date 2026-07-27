-- 0639: rimuove gli oggetti DB rimasti orfani dopo la cancellazione della
-- macchina Shadow dal codice.
--
-- La modalita' SHADOW rigiocava un run "in ombra" per confrontarlo col primario
-- durante il porting Python->Rust. Da quando la mig 0609 ha rimosso il selettore
-- di motore, l'entry point non aveva piu' chiamanti; il codice e' stato eliminato
-- (commit "refactor(shadow): rimossa la macchina Shadow"). Questi due oggetti
-- sono cio' che resta nello schema: nessuno li scrive, nessuno li legge.
--
-- Regola H: un fix e' definitivo se sopravvive a un wipe del DB e alla
-- riapplicazione delle migrazioni. Lasciarli qui significherebbe che ogni DB
-- ricostruito da zero nasce con una tabella e un setting che il codice non
-- conosce piu' - lo stesso genere di divergenza silenziosa che la 0637 ha dovuto
-- riparare. Le migrazioni 0453 e 0459 NON si toccano (sono immutabili: il loro
-- checksum e' registrato sui DB gia' migrati); il loro effetto si annulla qui.

-- ── nexus_shadow_telemetry (creata dalla mig 0453) ──────────────────────────
-- Conteneva il diff fra lo stato finale del run primario e quello dello shadow.
-- Il suo unico scrittore era `persist_node_diff` in
-- crates/nexus-agent-graph/src/shadow/mod.rs, file ora eliminato; non e' mai
-- esistito un lettore applicativo (la si interrogava a mano in SQL).
--
-- NOTA sui dati: sul DB di sviluppo la tabella conteneva 14 righe del 26-27
-- giugno 2026, telemetria del confronto Python<->Rust di quel periodo. Sono
-- diagnostica storica di un confronto che non ha piu' i due termini di paragone:
-- il DROP le elimina. Decisione presa consapevolmente, non un effetto collaterale.
DROP TABLE IF EXISTS nexus_shadow_telemetry;

-- ── agent.shadow.replay_turn_gap_ms (seminato dalla mig 0459) ───────────────
-- Serviva a `ReplayLlmGateway` per raggruppare gli step del primario in turni
-- durante il replay. Rimosso il gateway di replay, la chiave non ha piu' alcun
-- lettore: un flag che nessun ramo legge e' configurazione morta (regola G) e
-- confonde chi lo trova nel pannello admin.
DELETE FROM settings WHERE key = 'agent.shadow.replay_turn_gap_ms';
