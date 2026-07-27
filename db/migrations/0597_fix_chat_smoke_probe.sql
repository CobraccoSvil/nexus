-- 0597: ripara il probe chat_smoke e i verdetti che ha prodotto.
--
-- ROOT CAUSE 1 (codice, gia' corretta): `evaluate_attempt` leggeva il testo del
-- turno da `turn["result"]`, ma l'unico produttore (`agent_turn_value_from_gw`
-- in neural_client.rs) scrive `turn["content"]`. `content_chars` era quindi 0
-- per COSTRUZIONE e il predicato `min_content_chars: 1` non era soddisfacibile
-- da ALCUN modello. Misurato sul campo: mistral-medium-3.5, codestral-2508,
-- open-mistral-nemo e x-ai/grok-4.5 rispondono 'ok' in meno di 2s alla
-- richiesta IDENTICA del probe, e sono stati squalificati per "empty_content".
-- Con `enforce_routing_gate=true` (mig 0595) ogni squalifica ESCLUDE il modello
-- dal routing: la batteria stava smontando il parco 4 modelli per giro.
--
-- ROOT CAUSE 2 (configurazione, qui): `max_tokens: 64` non basta a un modello
-- con thinking per arrivare a scrivere "ok" — il ragionamento interno consuma
-- l'intero budget e la risposta esce vuota (finish_reason=length ->
-- error_class=empty_completion). Misurato: z-ai/glm-5.2 con max_tokens=64
-- fallisce, con 512 risponde 'ok'. Il probe misurava il budget che gli davamo,
-- non il modello. 512 e' il valore verificato sul campo; resta ampiamente sotto
-- i profili agentici (4096) perche' chat_smoke prova solo la raggiungibilita'.
--
-- NON e' un innalzamento di limite per nascondere una latenza (regola H): e' la
-- rimozione di una condizione di prova che rendeva il verdetto inattendibile.

UPDATE ai_model_probe_profile
   SET payload = jsonb_set(payload, '{max_tokens}', '512'::jsonb)
 WHERE profile_key = 'chat_smoke'
   AND (payload->>'max_tokens')::int < 512;

-- I verdetti prodotti dal probe rotto sono dati corrotti da un bug, non misure.
-- Vengono RIPORTATI A "DA PROVARE" (unqualified), NON promossi a qualified: la
-- qualifica si guadagna solo passando la batteria (il CHECK
-- chk_qualified_implies_probe lo impone). Azzerando backoff e attempts la
-- batteria — ora corretta — li rivaluta al prossimo giro e decide onestamente.
-- Perimetro CHIRURGICO: solo i verdetti prodotti dai due difetti sopra, sul
-- profilo chat_smoke. Ogni altra squalifica (di altri profili, o di chat_smoke
-- per ragioni diverse) resta intatta: se un modello e' davvero rotto, deve
-- restare fuori.
UPDATE ai_price_catalog
   SET qualification_state       = 'unqualified',
       qualification_reason      = 'requalify:probe_chat_smoke_fixed_mig0597',
       qualification_backoff_until = NULL,
       qualification_attempts    = 0,
       qualification_started_at   = NULL
 WHERE qualification_state = 'disqualified'
   AND (qualification_reason LIKE 'chat_smoke:empty_content:%'
     OR qualification_reason = 'chat_smoke:error_class:empty_completion');

-- L'evidenza in ai_model_probe_evidence NON viene cancellata: e' append-only ed
-- e' la prova storica del difetto (verdict=fail con content_chars=0 e
-- stop_reason=end_turn su modelli che rispondevano). Serve a chi indaghera' in
-- futuro; le nuove righe la superano per suite/attempt.
