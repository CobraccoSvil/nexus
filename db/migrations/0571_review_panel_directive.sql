-- 0571_review_panel_directive.sql
-- Attivazione della REVIEW ADVERSARIALE a valle (Fase C ultracode): innesco.
--
-- Il meccanismo esisteva gia' ma era DORMIENTE: compose_panel_verdict e' agganciato
-- al fan-in di dispatch_subagents (subagent_native.rs), il subagente `review' e'
-- abilitato con review_verdict, la QuorumPolicy (veto avversario) e' letta dal DB
-- (orchestrator.review_quorum_min_valid / review_fail_on_high_severity). Mancava
-- solo la DIRETTIVA che istruisce il run principale a convocare il panel: a
-- differenza del consiglio a monte (<consiglio_analisi>, mig 0549) non ne aveva
-- una, quindi 0 panel di review erano mai stati eseguiti.
--
-- Aggiunge <revisione_finale> in coda ai system prompt agente principali, come
-- <consiglio_analisi> (mig 0549) e <safety_progetto> (mig 0096). Simmetrico al
-- consiglio ma A VALLE: dopo aver implementato, PRIMA di dichiarare done, convoca
-- revisori READ-ONLY che chiudono con review_verdict; il coordinatore aggrega in
-- panel_verdict (veto della minoranza-con-evidenza). Canale corretto per il
-- comportamento agente (regola D). La condizione "modifica non banale" e'
-- post-hoc (l'agente valuta cosa ha fatto), quindi la direttiva e' sempre
-- presente sui prompt coder: e' la sua stessa condizione interna a evitare il
-- costo sui task banali.
--
-- Append idempotente: sentinel string guarda i duplicati su re-run.

DO $$
DECLARE
    sentinel TEXT := '<!-- 0571:review_panel -->';
    rules_block TEXT := E'\n\n<!-- 0571:review_panel -->\n<revisione_finale>\nREVISIONE AVVERSARIA A VALLE (panel di review indipendente prima di dichiarare done).\n\nQuando hai IMPLEMENTATO una modifica al codice NON banale — piu'' file, logica non\ntriviale, schema o dati DB, autenticazione o sicurezza, refactoring — PRIMA di\nchiamare task_complete con outcome=done convoca un panel di revisione: in UN solo\nbatch (dispatch_subagents) dispaccia uno o piu'' sub-agenti kind=review, READ-ONLY,\nindicando nel task i FILE che hai modificato e cosa devono verificare. I revisori\nesaminano il codice e chiudono con review_verdict (pass | fail | needs_changes +\nfindings con severity ed evidenza concreta).\n\nIl tool_result del batch include panel_verdict, il verdetto AGGREGATO (segnale\nstrutturato, non prosa):\n- pass: procedi con task_complete done.\n- needs_changes: applica le correzioni indicate dai findings (prima le severity\n  alta), poi ri-convoca il panel; se non puoi risolvere, dichiara l''esito\n  onestamente (partial o blocked), mai done spacciato per completo.\n- fail: NON dichiarare done. Correggi i difetti bloccanti e ri-verifica. Un solo\n  revisore che trova un difetto grave con evidenza (severity alta) fa fallire il\n  panel anche in minoranza: ha ragione.\n- inconclusive: il panel non ha raggiunto il quorum di voti validi; non trattarlo\n  come pass, ri-convoca.\n\nPer una modifica BANALE (un typo, una singola riga isolata) NON convocare il\npanel: sarebbe costo inutile. Convoca la revisione SOLO dal run principale, MAI\ndentro un sub-agente (niente ricorsione).\n</revisione_finale>';
BEGIN
    UPDATE nexus_prompt_templates
    SET content = content || rules_block
    WHERE key = 'system.nexus_base'
      AND is_active = TRUE
      AND content NOT LIKE '%' || sentinel || '%';

    UPDATE nexus_prompt_templates
    SET content = content || rules_block
    WHERE key = 'agent.coder.base'
      AND is_active = TRUE
      AND content NOT LIKE '%' || sentinel || '%';

    RAISE NOTICE 'Migrazione 0571 applicata: direttiva <revisione_finale> sui system prompt agente';
END
$$;
