-- 0614_time_grace_pct.sql
-- Turno di grazia a TEMPO: una figura che sta per esaurire il budget viene
-- SOLLECITATA a chiudere col proprio canale di ruolo, invece di essere uccisa muta.
--
-- Root cause: il tetto di una figura e' applicato da un `tokio::time::timeout`
-- ESTERNO al motore (subagent_native.rs): allo scadere il future viene droppato
-- senza negoziare, quindi la figura non ha MAI un turno per chiamare
-- advisory_verdict. Il consiglio la contava come astensione muta (nel display
-- "tempo scaduto"), buttando via il lavoro gia' svolto.
--
-- Meccanica: il budget della figura ora ENTRA nel motore
-- (NativeRunInput.run_time_budget_s -> ExecutorConfig.run_time_budget_s: prima il
-- gate a tempo dell'executor leggeva solo il setting globale
-- agent.run_time_budget_s, che e' 0 per policy, quindi era codice morto). Oltre
-- questa percentuale il gate, invece di chiudere, concede UN turno di grazia
-- (punto unico maybe_advisory_grace_delta, one-shot, gia' usato dal backstop
-- text-only) con la direttiva "chiudi ORA anche se parziale". Il ramo di chiusura
-- a budget esaurito resta invariato.
--
-- Il valore giusto dipende dalla latenza reale delle chiamate: il residuo
-- (100 - pct) deve bastare per UN turno piu' la chiamata del tool. Con chiamate
-- lente va abbassato. 0 = disabilitato (comportamento bit-identico a prima).

INSERT INTO settings (key, value, category, description) VALUES
  ('agent.time_grace_pct', '70', 'agent',
   'Percentuale del budget di tempo del run oltre cui un canale di ruolo ancora muto (figura del consiglio senza advisory_verdict, avvocato senza debate_position) riceve UN turno di grazia per dichiarare il proprio verdetto, invece di essere ucciso muto allo scadere. Il residuo (100 - pct) deve bastare per un turno piu'' la chiamata del tool: con chiamate lente va abbassato. 0 = disabilitato. DB-driven, regola G.')
ON CONFLICT (key) DO NOTHING;
