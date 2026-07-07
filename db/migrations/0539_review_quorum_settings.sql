-- 0539: policy del quorum del PANEL di review (Fase C ultracode).
--
-- Quando un batch di sub-run (dispatch_subagents) contiene revisori che
-- dichiarano un verdetto strutturato via il tool `review_verdict` (Fase B), il
-- coordinatore compone un verdetto di PANEL aggregato dai segnali strutturati
-- outcome.review (regola M, mai dalla prosa) e lo espone nel tool_result come
-- campo `panel_verdict`. Il punto unico PURO e' decisions::compose_panel_verdict
-- (nexus-agent-graph); la policy arriva come parametro (regola G: DB-driven).
--
-- I default nel codice coincidono coi valori qui sotto (QuorumPolicy::default)
-- come safe-default se la riga manca:
--   - review_quorum_min_valid: minimo numero di VOTI VALIDI (revisore arrivato a
--     esito + review dichiarato) perche' il panel sia conclusivo; sotto soglia il
--     verdetto e' 'inconclusive' (il coordinatore NON lo tratta come pass). Un
--     revisore in timeout/errore NON vota (astensione). Default 1.
--   - review_fail_on_high_severity: veto avversario. 'true' -> un solo verdetto
--     'fail' con almeno un finding severity 'alta' basta a far fallire il panel
--     (un revisore che trova un difetto grave con evidenza ha ragione anche in
--     minoranza). 'false' -> un fail vale come voto ordinario. Default 'true'.
INSERT INTO settings (key, value, category, description) VALUES
  ('orchestrator.review_quorum_min_valid', '1', 'orchestrator',
   'Fase C: minimo numero di voti VALIDI (revisore con esito + review dichiarato) perche'' il panel di review sia conclusivo; sotto soglia il verdetto e'' inconclusive (mai trattato come pass). DB-driven, regola G.'),
  ('orchestrator.review_fail_on_high_severity', 'true', 'orchestrator',
   'Fase C: veto avversario del panel. true = un solo verdetto fail con un finding severity alta fa fallire il panel anche in minoranza; false = il fail vale come voto ordinario. DB-driven, regola G.')
ON CONFLICT (key) DO NOTHING;
