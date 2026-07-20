-- 0621_verdetto_solo_dal_tool.sql
-- Toglie l'ESCA dal prompt delle figure del consiglio: il punto (5) di
-- <output_format> chiedeva "verdetto: proceed | proceed_with_changes | block"
-- DENTRO il final_answer, cioe' in prosa. Un modello che obbedisce a quella
-- riga considera il verdetto consegnato e non chiama advisory_verdict: la
-- figura chiude CompletedNoAdvisory e il quorum la perde. Misurato: 14 dei 24
-- run muti storici sono "prosa completa, tool mai chiamato" (run 20/07 10:03:
-- diagnosi corretta nel summary, parere mai dichiarato, consiglio inconclusive).
--
-- Il blocco <verdetto_strutturato> (mig 0548) dichiara gia' che "il parere
-- macchina e' SOLO quello del tool": questa riga lo contraddiceva. Il punto (5)
-- diventa il rimando esplicito al tool. Complemento del fix motore (turno di
-- grazia FORZANTE sulla chiusura muta): riduce la frequenza con cui ci si
-- arriva, non lo sostituisce.
--
-- REPLACE mirato della stringa esatta: i template senza quella riga restano
-- intatti (WHERE la filtra). Version bump per invalidare le cache dei prompt.

UPDATE nexus_prompt_templates
SET content = REPLACE(
        content,
        '(5) verdetto: proceed | proceed_with_changes | block (block solo con evidenza di',
        '(5) il verdetto NON va scritto qui: dichiaralo ESCLUSIVAMENTE chiamando il tool advisory_verdict (block solo con evidenza di'
    ),
    version = version + 1,
    updated_at = NOW()
WHERE key LIKE 'subagent.%'
  AND is_active
  AND content LIKE '%(5) verdetto: proceed | proceed_with_changes | block (block solo con evidenza di%';

UPDATE nexus_prompt_templates
SET content = REPLACE(
        content,
        '(5) verdetto: proceed | proceed_with_changes | block',
        '(5) il verdetto NON va scritto qui: dichiaralo ESCLUSIVAMENTE chiamando il tool advisory_verdict'
    ),
    version = version + 1,
    updated_at = NOW()
WHERE key LIKE 'subagent.%'
  AND is_active
  AND content LIKE '%(5) verdetto: proceed | proceed_with_changes | block%';
