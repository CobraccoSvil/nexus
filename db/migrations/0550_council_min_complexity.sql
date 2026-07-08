-- 0550_council_min_complexity.sql
-- Chiude il debito "soglia programmatica" del consiglio di figure: rende
-- l'attivazione DETERMINISTICA invece che affidata al giudizio del modello.
--
-- Il gate (gate_council_directive, prompt_templates.rs, punto unico) calcola lo
-- score OGGETTIVO del messaggio utente con estimate_prompt_complexity (0-100,
-- punto unico) e, se e' SOTTO questa soglia, RIMUOVE la direttiva
-- <consiglio_analisi> (mig 0549) dal system prompt: un task banale (typo, singola
-- modifica) prende il percorso agentico diretto, senza convocare il consiglio.
-- Sopra soglia la direttiva resta e il modello convoca le figure.
--
-- DB-driven (regola G): niente soglia hardcoded nel codice. Default 40 se la riga
-- manca (safe-default nel codice). Tunabile a caldo (cache settings ~60s).

INSERT INTO settings (key, value, category, description) VALUES
  ('orchestrator.council_min_complexity', '40', 'orchestrator',
   'Consiglio a monte: score MINIMO di complessita'' (0-100, da estimate_prompt_complexity) perche'' la direttiva <consiglio_analisi> resti nel system prompt. Sotto soglia la direttiva viene rimossa e il task prende il percorso agentico diretto (niente consiglio su task banali). DB-driven, regola G.')
ON CONFLICT (key) DO NOTHING;
