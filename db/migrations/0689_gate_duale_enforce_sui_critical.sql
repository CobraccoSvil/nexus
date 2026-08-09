-- 0689: il gate duale passa da enforce_irreversible a enforce (convoca anche sui Critical)
--
-- ROOT CAUSE
-- Il gate duale (due validatori su provider distinti prima di eseguire un passo
-- pericoloso) e' deployato dal 06/08 e NON E' MAI SCATTATO IN ESERCIZIO.
-- MISURATO il 09/08/2026 sui DB di progetto: `nexus_agent_meta_steps` con
-- kind='step_validation' ha 45 righe totali, l'ultima del 08/08 alle 10:40,
-- cioe' dello sviluppo del gate stesso. In un run agentico completo di quel
-- giorno (piano con `dotnet ef database update` + seed + build + avvio servizi)
-- le convocazioni sono state ZERO.
--
-- Le cause erano DUE e la mig 0688 ne ha chiusa una sola:
--   (a) il livello base nasceva da `is_mutator_tool_name`, dentro-o-fuori, e
--       `Mutating` non convoca in nessuna modalita'. Chiusa da 0688, che deriva
--       il pavimento dalla PORTATA del passo (`step_reach`): una `run_command`
--       che non si e' potuta collocare vale `unconfined` -> pavimento Critical.
--   (b) la MODALITA' e' rimasta `enforce_irreversible`, che convoca sui soli
--       Irreversible. Con (a) chiusa, `dotnet ef database update` ora vale
--       Critical -- e continuerebbe a non convocare nessuno. Questa migrazione
--       chiude (b).
--
-- PERCHE' ADESSO E NON IL 06/08
-- L'interruttore era rimasto su `enforce_irreversible` per una ragione di costo
-- che era REALE: senza una soglia, ogni `run_command` avrebbe pagato due
-- chiamate LLM, incluso un `ls`. La 0688 introduce quella soglia
-- (`orchestrator.step_reach.observation_commands`): i comandi provatamente
-- innocui sono assolti PRIMA della convocazione. La taratura che il piano
-- d'origine poneva come condizione per il passaggio a `enforce` esiste ora.
--
-- COSTO: NON MISURATO, E VA LETTO NON STIMATO
-- La sola misura disponibile e' PRE-soglia (235 batch su 673 in 4 giorni su
-- gestione-corsi, 35%, contenevano un passo non confinato) e NON descrive il
-- costo del gate acceso: e' stata raccolta prima che le osservazioni fossero
-- assolte. Il tasso reale si legge dalle righe `step_validation` che questa
-- migrazione fara' nascere, contando le convocazioni per run -- non si stima.
--
-- ROLLBACK (a caldo, senza redeploy; la cache settings ha TTL 60s)
--   UPDATE settings SET value = 'enforce_irreversible'
--    WHERE key = 'orchestrator.critical_step_gate_mode';
-- Se invece a essere rumoroso e' un comando preciso, il rimedio NON e' spegnere
-- il gate: e' aggiungere quel comando al vocabolario
-- `orchestrator.step_reach.observation_commands` (regola H).

UPDATE settings
   SET value = 'enforce',
       updated_at = NOW()
 WHERE key = 'orchestrator.critical_step_gate_mode'
   AND value = 'enforce_irreversible';

DO $$
DECLARE
  v_modo TEXT;
BEGIN
  SELECT value INTO v_modo
    FROM settings
   WHERE key = 'orchestrator.critical_step_gate_mode';

  IF v_modo IS NULL THEN
    RAISE NOTICE '0689: chiave critical_step_gate_mode ASSENTE: il gate resta al suo default di codice. Verificare la mig che la introduce.';
  ELSIF v_modo <> 'enforce' THEN
    -- Non forziamo: se un operatore l'ha portata altrove di proposito
    -- (es. 'observe' per una taratura in corso) questa migrazione non deve
    -- sovrascrivere quella decisione.
    RAISE NOTICE '0689: modo = %, diverso da enforce_irreversible: lasciato invariato.', v_modo;
  END IF;
END $$;
