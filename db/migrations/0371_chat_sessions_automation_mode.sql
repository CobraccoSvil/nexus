-- 0371_chat_sessions_automation_mode.sql
-- Modalita' di automazione PER-SESSIONE (fonte unica, regola G).
--
-- Bug: la modalita' (study/confirm/automatic) viveva solo nello stato React +
-- localStorage globale lato UI, con 4 default 'confirm' sparsi nel backend e un
-- hardcode AutomationMode::Confirm in process_resume.rs. Risultato: un run poteva
-- nascere 'confirm' anche con il dropdown su Automatico (es. dopo un reload della
-- UI che resetta lo stato), e i run risvegliati (process_resume, service_observer)
-- perdevano la modalita' scelta -> l'agente chiedeva conferme/chiarimenti che in
-- Automatico non dovrebbe.
--
-- Fix: la modalita' diventa una colonna persistita sulla sessione. send_chat_message
-- la scrive a ogni invio esplicito; process_resume / service_observer la LEGGONO
-- invece di hardcodare. Il default vive SOLO nel DEFAULT della colonna (niente piu'
-- default a cascata nel codice). Idempotente.

ALTER TABLE chat_sessions
    ADD COLUMN IF NOT EXISTS automation_mode TEXT NOT NULL DEFAULT 'confirm';
