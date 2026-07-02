-- 0506: prevenzione anti-loop nei system prompt + iteration_cap DB-driven
-- (ADR 0035 — filosofia anti-loop: misurare il progresso, non la ripetizione).
--
-- 1. La sezione <anti_loop> dei template agente istruiva ad ARRENDERSI
--    ("se dopo 2 iterazioni non c'e' avanzamento, INTERROMPI e riporta"):
--    l'opposto del comportamento desiderato e la causa a monte degli stalli
--    che i detector runtime devono poi gestire d'emergenza. Riscritta con la
--    gerarchia: cambia UNA cosa -> cambia STRATEGIA -> dichiara blocked con
--    task_complete (mai resa in prosa). E' la stessa gerarchia dei nudge
--    runtime del progress_controller (GUIDE -> FORCE_DIAGNOSE ->
--    CHANGE_STRATEGY -> escalation): la prevenzione nel prompt agisce PRIMA
--    dello stallo, i nudge restano la rete quando il modello la ignora.
--
-- 2. agent.executor.iteration_cap: il doc di ExecutorConfig la dichiarava
--    DB-driven ma il loader non leggeva alcuna chiave (restava la costante
--    60). Ora load_executor_config legge questa chiave (safe-default 60).

UPDATE nexus_prompt_templates
   SET content = regexp_replace(
         content,
         '<anti_loop>.*?</anti_loop>',
         '<anti_loop>' || chr(10) ||
         'Ripetere identico non e'' una strategia. Gerarchia obbligatoria davanti a un errore o a un esito che non cambia:' || chr(10) ||
         '1. Dopo un errore NON rieseguire la stessa chiamata identica: cambia UNA cosa precisa (input, parametro, bersaglio) in base all''errore osservato.' || chr(10) ||
         '2. Se dopo 2 tentativi mirati l''esito e'' ANCORA lo stesso, CAMBIA STRATEGIA restando sul task: strumento diverso (es. write_file col contenuto completo invece di edit_file), piu'' contesto (leggi il file intero, i log, l''errore completo), oppure decomponi il problema in un passo piu'' piccolo e verificabile.' || chr(10) ||
         '3. Un comando di verifica (build/test) rilanciato dopo OGNI correzione non e'' una ripetizione: e'' il ciclo corretto. Rilanciarlo senza aver cambiato nulla in mezzo si''.' || chr(10) ||
         '4. Solo se ogni strada e'' impedita da una causa ESTERNA (credenziale, permesso, servizio non disponibile, dipendenza mancante), dichiara l''esito con task_complete (outcome=blocked + blocker): MAI arrendersi in prosa e MAI dichiarare done senza verifica.' || chr(10) ||
         '</anti_loop>',
         ''
       ),
       updated_at = NOW(),
       version = version + 1
 WHERE content LIKE '%<anti_loop>%';

INSERT INTO settings (key, value, description)
VALUES (
  'agent.executor.iteration_cap',
  '60',
  'Safety net finale del run agente: iterazioni executor oltre cui il run chiude deterministicamente (forced_close). Era una costante dichiarata DB-driven ma mai letta; ora load_executor_config la legge (mig 0506, ADR 0035). Cache 60s.'
)
ON CONFLICT (key) DO NOTHING;
