-- Il panel di review: o zero, o un quorum. Mai uno.
--
-- Il profilo LOW chiedeva 1 revisore. Un panel da UNO e' il peggiore dei tre
-- stati possibili: costa quanto una review vera (un sub-run intero, con i suoi
-- token e il suo timeout), parla con l'autorita' di un quorum -- puo' bocciare
-- il lavoro e rimandarlo in correzione fino a
-- `orchestrator.review_max_correction_cycles` -- ma ha la base di un giudizio
-- unico. Con `orchestrator.review_quorum_min_valid` a 1 quel voto solo non e'
-- nemmeno mai `Inconclusive`: decide da solo.
--
-- E' lo stesso danno gia' visto sull'altro asse il 26/07 (dieci panel di review
-- consecutivi, tutti sullo stesso identico modello, con l'apparenza di una
-- verifica plurale), raggiunto per un'altra strada: non un giudice duplicato,
-- un giudice solo.
--
-- Per un task a complessita' bassa la risposta onesta e' ZERO: nessuna review
-- programmatica, resta la direttiva `<revisione_finale>` del prompt agente.
-- Medium e high chiedono gia' 2 e restano invariati. Il floor di quorum della
-- review passa da 1 a 2 nel codice (`panel_floor`, crate nexus-agent-graph),
-- cosi' il degrado a budget stretto la porta a 0 invece che a 1 -- il principio
-- "mai convocato monco" che la mig 0602 dichiarava gia' per gli altri panel.
--
-- La nota della 0602 ("con il gate deliberate attuale i task low non convocano
-- panel; il profilo esiste per completezza") e' superata dai fatti: il
-- 2026-07-27 un task low ha convocato il gate con `reviewers=1`. Il profilo e'
-- vivo, e va detto cosa significa.

UPDATE settings
   SET value = '{"council_figures":1,"reviewers":0,"providers":0,"advocates":0}',
       description = 'Profilo di DOMANDA per task a complessita'' LOW (JSON: '
                     'council_figures, reviewers, providers, advocates). '
                     'reviewers=0 per scelta: un panel di review da UNO costa '
                     'quanto un quorum, decide come un quorum e non lo e'' '
                     '(vedi mig 0643). Per la review vale "o zero o almeno '
                     'due"; il floor di quorum e'' nel codice (panel_floor).'
 WHERE key = 'orchestrator.sizing_profile_low';
