-- 0660_thinking_matrix_senza_gate_circolare.sql
--
-- La misura deve raggiungere chi l'euristica classifica male.
--
-- Il profilo `thinking_matrix` (mig 0594) nasceva gated su
-- `applies_when = {"declared_capabilities_contains": "reasoning"}`, cioe' girava
-- solo sui modelli che DICHIARAVANO reasoning. Ma `capabilities` non e' un dato
-- osservato: nasce da `base_caps_from_name`, che per google legge il NOME — con
-- "pro" produce ["reasoning", ...], con "flash-lite" produce ["chat","simple"].
--
-- Il gate era percio' circolare: il nome decideva chi meritasse di essere
-- misurato, e la misura era l'unica cosa capace di correggere il nome. La
-- matrice girava esattamente e soltanto dove l'euristica era gia' d'accordo con
-- se stessa, quindi non poteva smentirla in nessun caso.
--
-- Misurato il 30/07/2026: gemini-3.1-pro-preview misurato 24 volte e portato a
-- 'none' DAI FATTI; gemini-3.1-flash-lite mai misurato una volta, fermo alla
-- policy indovinata dal nome. In tutto 85 candidati su 110 avevano una
-- `agentic_thinking_policy` scritta e zero evidenze a sostenerla, inclusa
-- l'intera famiglia gpt-5.x e tutte le righe 'native'.
--
-- Il gate non serviva nemmeno come filtro di costo: i candidati della batteria
-- sono gia' ristretti da `nexus-model-eligibility::CONDITIONS`
-- (`is_enabled AND supports_tool_use`), cioe' sono gia' esattamente i modelli
-- per cui `agentic_thinking_policy` ha un significato. `applies_when` assente
-- significa "si applica a ogni candidato" (`profile_applies`, model_qualification.rs).
--
-- La copertura e' in `la_matrice_thinking_misura_anche_chi_il_nome_non_dice_reasoning`
-- (model_qualification.rs), che legge il profilo dal DB migrato con la stessa
-- `load_profiles` della produzione e lo passa a `profile_applies`, il punto che
-- decide davvero: rimettendo qui l'`applies_when`, quel test rosseggia.

UPDATE ai_model_probe_profile
   SET applies_when = NULL
 WHERE profile_key = 'thinking_matrix';
