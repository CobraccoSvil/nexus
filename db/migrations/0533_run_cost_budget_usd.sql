-- 0531: budget di run in DOLLARI (price-aware). Il tetto anti-runaway del run era
-- solo in TOKEN (agent.run_token_budget = 400000, mig 0520), cieco al prezzo: 400k
-- token costano ~$0.07 su deepseek-v4-flash ma ~$4.5 su gpt-5.5 (64x). Un run su
-- modello caro poteva quindi spendere decine di volte tanto senza che nulla lo
-- frenasse (audit del flusso di selezione: nessun tetto in dollari sul run
-- principale, cost_cap_usd hardwired a 0).
--
-- Con questa chiave il tetto token EFFETTIVO del turno diventa il piu' stringente
-- tra agent.run_token_budget e il numero di token pari a run_cost_budget_usd al
-- prezzo blended (input*0.75 + output*0.25) del modello del turno. Effetto:
--   - modelli economici (deepseek-flash ~0.175 $/M): 3.0$/0.175 = ~17M token ->
--     resta il tetto token 400k (nessun taglio, l'economico usa tutto);
--   - modelli cari (gpt-5.5 ~11.25 $/M): 3.0$/11.25 = ~267k token -> il run si
--     ferma a ~$3 invece di ~$4.5, e ancora prima man mano che il prezzo sale.
-- Il tetto e' per TURNO-MODELLO; l'escalation "di un gradino" (audit fix 2) limita
-- quanto il prezzo puo' crescere entro il run.
--
-- Config DB-driven (regola G): letto da load_executor_config (cache settings). Il
-- default nel codice e' 0.0 (disattivato = bit-identico al solo tetto token): e'
-- QUESTA riga che attiva il freno. Abbassare per un tetto piu' aggressivo, alzare
-- (o '0') per allentarlo. Complementare a agent.run_token_hard_cap (backstop
-- assoluto non-negoziabile) e ad ai_quota_policies (quota per progetto/periodo lato
-- gateway).
INSERT INTO settings (key, value, category, description) VALUES
  ('agent.run_cost_budget_usd', '3.0', 'agent',
   'Tetto di costo in USD per turno-modello del run agentico: il budget token effettivo e'' min(agent.run_token_budget, run_cost_budget_usd / prezzo_blended_modello). Rende il tetto anti-runaway uniforme in dollari invece che in token (cieco al prezzo). 0 = disattivato (solo tetto token). DB-driven, regola G.')
ON CONFLICT (key) DO NOTHING;
