-- 0720_round_non_misurante.sql
-- Il round della batteria che NON ha potuto guardare il modello non e' un
-- tentativo del modello.
--
-- Fino a qui lo era: lo skip per cooldown del fornitore e il giro i cui
-- inconclusivi erano TUTTI attribuiti al fornitore passavano entrambi da
-- SQL_INCONCLUSIVE, che fa qualification_attempts + 1 e raddoppia il backoff
-- esponenziale. Un modello di un fornitore saturo accumulava cosi' giorni di
-- backoff senza che la batteria lo avesse mai interrogato. Il codice ora
-- distingue (EsitoInconcluso::RoundNotMeasuring, reason
-- 'round_not_measuring:*'): attempts invariato, backoff BREVE fisso.

-- (1) Il backoff breve del round non misurante (regola G: la config sta nel
--     DB; il default nel codice sta in nexus-model-eligibility accanto alle
--     chiavi sorelle, cosi' l'explain puo' dichiararlo).
INSERT INTO settings (key, value, category, description) VALUES
  ('agent.model_qualification.not_measuring_backoff_minutes', '60', 'agent',
   'Batteria di qualificazione: minuti di backoff FISSO dopo un round NON MISURANTE (fornitore in cooldown, o inconclusivi tutti attribuiti al fornitore). Un round simile non incrementa qualification_attempts: "non ho potuto guardare" non e'' un tentativo del modello.')
ON CONFLICT (key) DO NOTHING;

-- (2) RIPARAZIONE derivata dai fatti (regola H): azzera attempts/backoff SOLO
--     per chi ha alle spalle esclusivamente round non attribuibili al modello
--     — ultimo esito inconclusivo E nessuna evidenza 'fail' conclusiva mai
--     registrata per la coppia. Non e' una lista a mano: e' la stessa
--     condizione che il codice nuovo avrebbe applicato, proiettata sui dati
--     che il codice vecchio ha danneggiato. Conservativa nella direzione
--     giusta: il peggio che puo' accadere a un reset "di troppo" e' una
--     rimisura anticipata; un modello che ha MAI fallito conclusivamente
--     tiene il suo backoff. I filtri sono su qualification_reason, che e' un
--     identificatore canonico scritto dal codice (mai prosa): riparazione
--     one-shot, non una decisione a runtime (regola M rispettata).
UPDATE ai_price_catalog p
   SET qualification_attempts = 0,
       qualification_backoff_until = NULL
 WHERE p.qualification_attempts > 0
   AND p.qualification_reason IN ('inconclusive_round', 'provider_in_cooldown')
   AND NOT EXISTS (SELECT 1 FROM ai_model_probe_evidence e
                    WHERE e.provider = p.provider AND e.model = p.model
                      AND e.verdict = 'fail');
