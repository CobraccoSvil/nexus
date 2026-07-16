-- 0602_orchestration_sizing_settings.sql
-- Dimensionamento dell'orchestrazione multi-agente in base al problema
-- (resolver puro orchestration_sizing in nexus-agent-graph).
--
-- Root cause chiusa: i panel (consiglio, review adversariale, multi-provider)
-- erano dimensionati SOLO da cap fissi (council_max_figures=6,
-- review_panel_size=2, multi_provider 2/3) decisi dalla configurazione, non dal
-- problema. Da questa migrazione la DOMANDA viene dai profili per-classe qui
-- sotto (configurabili da admin), l'OFFERTA dal doppio vincolo budget di costo
-- + budget di tempo (vince il piu' stretto, campo strutturato `sized_by` nel
-- meta-step `orchestration_plan`), e i cap storici restano come BACKSTOP
-- assoluti sulle STESSE chiavi (nessuna seconda fonte di verita', regola L).
--
-- `orchestrator.sizing_enabled` nasce 'false': ogni fase del paradigma e'
-- bit-identica a flag OFF; l'attivazione complessiva avviene con la mig 0607
-- dopo la verifica E2E (il paradigma consegnato e' ATTIVO, non dormiente).
--
-- Idempotente: ON CONFLICT DO NOTHING (non sovrascrive valori gia' impostati).

INSERT INTO settings (key, value, category, description) VALUES
  ('orchestrator.sizing_enabled', 'false', 'orchestrator',
   'Dimensionamento dei panel multi-agente in base al problema (resolver orchestration_sizing): classe di complessita'' del classificatore + profilo per-classe + budget residuo costo/tempo decidono quante figure/revisori/provider/avvocati convocare. false = comportamento legacy coi soli cap fissi. Kill-switch (regola G); flip a true con mig 0607 dopo la E2E.'),
  ('orchestrator.sizing.budget_share_pct', '20', 'orchestrator',
   'Quota percentuale del budget di costo RESIDUO del run (agent.run_cost_budget_usd) spendibile nei panel pre/post-run. Il run principale deve restare finanziato: i panel consumano al massimo questa fetta. Clampata a 100 nel codice.'),
  ('orchestrator.sizing.panel_priority', 'council,multi_provider,debate,review', 'orchestrator',
   'Ordine di PRIORITA'' dei panel per il degrado a budget stretto (CSV canonico: council|multi_provider|debate|review). L''ULTIMO si sacrifica per primo; ogni panel scende prima al proprio floor di quorum, poi a 0 (mai convocato monco, lezione mig 0589). Token malformati ignorati; panel assenti accodati nell''ordine di default.'),
  ('orchestrator.sizing.est_subrun_tokens', '60000', 'orchestrator',
   'Token TOTALI attesi di un singolo sub-run advisory, usati per stimare il costo unitario (prezzo dal listino nexus-pricing del modello risolto VIA TIER del purpose della prima figura del consiglio; ripartizione 80/20 prompt/completion nel codice). Raffinamento futuro: mediana della telemetria reale dei sub-run.'),
  ('orchestrator.sizing.est_subrun_duration_s', '240', 'orchestrator',
   'Durata attesa (secondi) di un singolo sub-run advisory, usata per il vincolo di tempo: sub-run affordabili = (tempo residuo / durata) x parallelismo del fan-out. Seme = timeout tipico delle figure del consiglio (240s, mig 0546).'),
  ('orchestrator.sizing_profile_low', '{"council_figures":1,"reviewers":1,"providers":0,"advocates":0}', 'orchestrator',
   'Profilo di DOMANDA per task a complessita'' LOW (JSON: council_figures, reviewers, providers, advocates). NB: con il gate deliberate attuale i task low non convocano panel; il profilo esiste per completezza e per un eventuale gate futuro piu'' permissivo.'),
  ('orchestrator.sizing_profile_medium', '{"council_figures":3,"reviewers":2,"providers":2,"advocates":0}', 'orchestrator',
   'Profilo di DOMANDA per task a complessita'' MEDIUM (JSON: council_figures, reviewers, providers, advocates). Editabile dalla pagina admin Dimensionamento.'),
  ('orchestrator.sizing_profile_high', '{"council_figures":5,"reviewers":2,"providers":3,"advocates":2}', 'orchestrator',
   'Profilo di DOMANDA per task a complessita'' HIGH (JSON: council_figures, reviewers, providers, advocates). advocates si applica solo quando il consiglio dichiara una contested_decision (debate, mig 0605). Editabile dalla pagina admin Dimensionamento.'),
  ('orchestrator.sizing_budget_cost_usd_default', '2.50', 'orchestrator',
   'Budget di costo di default (USD) suggerito alla pagina admin Dimensionamento per i run senza budget esplicito. Informativo per la UI: il vincolo effettivo del resolver resta agent.run_cost_budget_usd.'),
  ('orchestrator.sizing_budget_time_s_default', '900', 'orchestrator',
   'Budget di tempo di default (secondi) suggerito alla pagina admin Dimensionamento. Informativo per la UI: il vincolo effettivo arriva con la deadline di run (agent.run_time_budget_s, mig 0604).')
ON CONFLICT (key) DO NOTHING;
