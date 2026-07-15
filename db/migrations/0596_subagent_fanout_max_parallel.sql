-- 0596_subagent_fanout_max_parallel.sql
-- Tetto di concorrenza del FAN-OUT di sub-run (consiglio, panel di review,
-- panel multi-provider) — punto unico spawn_fanout in subagent_native.rs.
--
-- Contesto (incidente consiglio 2026-07-15, difetto D3 PROVATO): i tre fan-out
-- lanciavano N sub-run come future dentro UN SOLO task tokio
-- (FuturesUnordered/join_all, nessun tokio::spawn). Con loro finivano nello
-- stesso task anche i loro tokio::time::timeout: un Timeout ritorna Elapsed
-- solo quando viene POLLATO, quindi un singolo membro che blocchi il thread
-- dentro il proprio poll() congelava TUTTI gli altri e i loro timer. Firma
-- misurata: 4 sub-run con t0 diversi e timeout_s=240 morti tutti allo stesso
-- millisecondo dopo 408s.
--
-- Ora ogni sub-run ha il PROPRIO task (spawn) e questo setting governa quanti
-- possono essere in volo insieme, con un SEMAFORO (il permesso si libera appena
-- un sub-run finisce; mai barriere a ondate, che ricreerebbero la firma "tutti
-- insieme").
--
-- Default 6 = il fan-out nominale del consiglio: NON introduce un tetto piu'
-- stretto del comportamento storico (nessuna regressione di latenza al deploy).
-- E' il knob da abbassare se in futuro si volesse limitare la pressione su un
-- singolo endpoint provider; alzarlo non serve (il consiglio ha 6 figure).

INSERT INTO settings (key, value, category, description) VALUES
  ('orchestrator.subagent_fanout_max_parallel', '6', 'orchestrator',
   'Massimo numero di sub-run di un FAN-OUT (consiglio delle figure, panel di review, panel multi-provider) realmente in volo insieme. Ogni sub-run gira nel proprio task tokio col proprio timeout; il semaforo libera il permesso appena uno finisce. Default 6 = fan-out nominale del consiglio (nessun tetto artificiale). DB-driven, regola G.')
ON CONFLICT (key) DO NOTHING;
