-- 0619_gateway_deterministic_streak.sql
-- Tetto sui turni consecutivi falliti al gateway con causa DETERMINISTICA.
--
-- Root cause (run 2abb30db, 20/07): quando il failover cross-provider non
-- scatta (cap escalation raggiunto, nessun sostituto sano, causa fuori dalla
-- whitelist di recupero), il turno d'errore sintetico lascia provider e model
-- sticky INVARIATI: il giro dopo rifa' la stessa chiamata e riceve la stessa
-- risposta degenere (empty_completion ~7s, 400 client ~500ms), fino a consumare
-- il budget. I retry intra-iterazione non emettono meta-step: dall'esterno il
-- run appare "al lavoro" mentre gira a vuoto — e' il "silenzio finale" delle
-- figure morte in timeout.
--
-- Meccanica: l'executor conta i turni consecutivi (iterazioni CONTIGUE) falliti
-- sulla stessa coppia provider/model con causa deterministica (empty_completion,
-- client_error non recuperabile). Alla soglia chiude con stop_reason=error e
-- error_class='gateway_deterministic' (segnale strutturato, regola M). Le cause
-- transitorie (cooldown, transient, billing) restano fuori dal tetto: possono
-- risolversi da sole. 0 = disabilitato, comportamento identico a prima.

INSERT INTO settings (key, value, category, description) VALUES
  ('agent.gateway_deterministic_streak_max', '3', 'agent',
   'Turni consecutivi falliti al gateway sulla stessa coppia provider/model con causa deterministica (empty_completion, client_error non recuperabile) oltre cui il run chiude con esito onesto invece di ritentare la stessa chiamata fino al budget. Le cause transitorie (cooldown, transient) sono escluse. 0 = disabilitato. DB-driven, regola G.')
ON CONFLICT (key) DO NOTHING;
