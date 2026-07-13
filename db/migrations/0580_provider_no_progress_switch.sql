-- 0580_provider_no_progress_switch.sql
--
-- 3.4 (difesa strutturale, dietro flag): quando il cap solo-testo scatta (un provider
-- non produce output utile per N turni consecutivi), invece di chiudere il run col
-- backstop, l'executor PROVA prima a CAMBIARE PROVIDER via failover (`failover_provider`),
-- escludendo i provider gia' provati. Cosi' un provider bloccato (es. che descrive senza
-- agire) non affossa il run se un altro puo' procedere.
--
-- Default OFF (regola: "dietro flag, default che replica il comportamento attuale finche'
-- validato"): con 'false' il comportamento e' BIT-IDENTICO (chiusura backstop come oggi).
-- Abilitare per validare in campo, poi eventualmente promuovere a default.

INSERT INTO settings (key, value, category)
VALUES ('agent.provider_no_progress.enabled', 'false', 'agent')
ON CONFLICT (key) DO NOTHING;
