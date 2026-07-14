-- 0589_council_advisory_quorum_pct.sql
-- Quorum RELATIVO del consiglio a monte (advisory panel).
--
-- Root cause (incidente 2026-07-14, run Beaty-Book): il quorum del panel advisory
-- era la sola soglia ASSOLUTA `council_advisory_min_valid` (=1, mig 0548) e il
-- denominatore era un proxy di presenza (`total_advisories` = pareri consegnati):
-- una figura in timeout/errore SPARIVA dal conteggio invece di pesare come
-- astensione. Con 4 figure su 5 senza esito il consiglio dichiarava
-- `verdict=proceed` e la sintesi iniettata affermava "parere convergente del
-- consiglio" su 1 voto, senza dichiarare la base (regola M violata: esito
-- affermato senza la base che lo giustifica).
--
-- Fix (codice, stesso commit): `compose_advisory_synthesis` riceve il roster
-- CONVOCATO come input esplicito (`AdvisoryRoster::Convened(n)`) e la soglia
-- effettiva diventa `max(min_valid, ceil(convocate * quorum_pct / 100))` (punto
-- unico `panel_quorum::required_valid`). Sotto soglia il verdetto e'
-- `inconclusive` e il blocco iniettato dichiara "X pareri su N convocate, quorum
-- non raggiunto: pareri PARZIALI" (decisione utente: opzione b, iniezione onesta;
-- il consiglio resta advisory e non blocca il run).
--
-- Questa chiave e' il quorum percentuale (0-100) sulle figure convocate. Il
-- safe-default nel codice (AdvisoryPolicy::default) coincide con questo valore
-- se la riga manca (pattern mig 0548); il DB resta la fonte di verita' (regola G).
--
-- Idempotente: ON CONFLICT DO NOTHING.

INSERT INTO settings (key, value, category, description) VALUES
  ('orchestrator.council_advisory_quorum_pct', '50', 'orchestrator',
   'Consiglio a monte: percentuale (0-100) delle figure CONVOCATE che deve produrre un parere valido perche'' il panel advisory sia conclusivo. Soglia effettiva = max(council_advisory_min_valid, ceil(convocate * pct / 100)); sotto soglia il verdetto e'' inconclusive e la sintesi iniettata dichiara la parzialita'' (mai un consenso non deliberato). DB-driven, regola G.')
ON CONFLICT (key) DO NOTHING;
