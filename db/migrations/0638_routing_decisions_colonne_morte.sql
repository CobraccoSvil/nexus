-- 0638: rimuove da nexus_routing_decisions due colonne che nessuno scrive e
-- nessuno legge, e corregge la documentazione di una terza.
--
-- 0112 e' immutabile, quindi la correzione va qui.
--
-- L'unico scrittore della tabella e' `spawn_routing_decision_insert`
-- (crates/mcp-core/src/orchestrator/intent.rs), fire-and-forget. Nel repository
-- non esiste una sola SELECT su questa tabella: si interroga a mano in SQL per
-- audit, drift del classificatore e mix di costo.
--
-- ORDINE DI DEPLOY: la patch Rust che toglie `classifier_cached` dalla lista
-- colonne dell'INSERT viaggia nello stesso commit di questa migrazione. Un
-- rollback del solo binario, con la migrazione gia' applicata, romperebbe
-- l'INSERT — e siccome l'errore e' solo un warn!, la telemetria morirebbe in
-- silenzio.

-- actual_quality_score: 0112 la giustificava con una fase di analytics offline
-- che non e' mai esistita. Nessuno la scrive (l'INSERT non la elenca) e nessuno
-- la legge. Se un giorno servira' un punteggio di qualita', si aggiungera'
-- insieme al codice che lo calcola.
ALTER TABLE nexus_routing_decisions DROP COLUMN IF EXISTS actual_quality_score;

-- classifier_cached: 0112 la elencava sotto "output del classifier", ma il
-- codice la legava esplicitamente a NULL con il commento "non noto a questo
-- livello". Se il dato serve, va portato fin qui dal classificatore come
-- segnale strutturato, non lasciato come colonna sempre vuota.
ALTER TABLE nexus_routing_decisions DROP COLUMN IF EXISTS classifier_cached;

COMMENT ON TABLE nexus_routing_decisions IS
'Telemetria append-only delle decisioni di routing: una riga per risoluzione provider/modello, scritta in fire-and-forget per non aggiungere latenza al percorso caldo. Nessun componente applicativo la legge.';

COMMENT ON COLUMN nexus_routing_decisions.classifier_source IS
'Assume due soli valori: llm quando classifier_confidence e'' almeno 0.85, altrimenti keyword_or_promotion. E'' una deduzione dalla soglia fatta da chi scrive la riga, non un segnale strutturato ricevuto dal classificatore: il percorso keyword e quello di promozione agentica non sono distinguibili qui.';
