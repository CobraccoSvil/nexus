-- 0681 — Il dialogo frontend<->backend osservato da un browser reale.
--
-- ROOT CAUSE (misurata il 06/08/2026 su biblioteca-scolastica). L'app e' stata
-- dichiarata completa da QUATTRO run diversi mentre nel browser falliva ogni
-- chiamata. Il final_gate sondava il backend con reqwest e vedeva verde a
-- ragione: `curl :35976/health` -> 200, `curl :35976/api/books` -> 200, il
-- backend funzionava davvero. Le due cause del guasto gli erano invisibili per
-- COSTRUZIONE, non per svista:
--   - CORS assente sul backend: reqwest non manda `Origin` e non applica la
--     same-origin policy, quindi riceve 200 dove il browser blocca. Aggiungere
--     l'header alla probe non chiuderebbe il buco: manca il motore che APPLICA
--     la policy.
--   - URL costruito a runtime dal client (`/api/api/books`, doppio prefisso):
--     la probe prova l'URL DICHIARATO dall'agente, e non esegue JS.
-- In piu' Tailwind era dichiarato, installato e mai configurato: nessun
-- layout. La lente che lo rileva (`ui_styling_audit`) esisteva gia' ed era
-- senza effetto: e' un tool offerto a due figure e nessun gate la interroga.
-- E' la lezione che questa migrazione applica: una misura che nessun gate
-- interroga si e' costruita, non e' entrata in esercizio.
--
-- IL CRITERIO E' AGNOSTICO ALLO STACK per costruzione: osserva il SINTOMO (una
-- pagina che non ottiene i propri dati), mai il meccanismo che lo produce
-- (proxy Vite, rewrite di Next, middleware .NET, WhiteNoise di Django).
-- Inseguire le architetture a codice sarebbe la toppa che la regola H vieta.
-- Il browser e' Chromium headless dalla cache di Nexus, con NODE_PATH sulla
-- node_modules di Nexus: il progetto osservato non deve avere playwright, npm
-- ne' alcuna dipendenza (stessa scelta gia' in esercizio in visual_compare).
--
-- Punto unico del criterio: nexus-agent-graph/src/decisions/browser_dialogue.rs
-- (puro); confine col browser: mcp-core/src/agent_tools/browser_probe.rs.
--
-- ROLLBACK: UPDATE settings SET value='false'
--           WHERE key='agent.final_gate.browser_dialogue_enabled';

INSERT INTO settings (key, value, category, description, is_secret)
VALUES
  (
    'agent.final_gate.browser_dialogue_enabled',
    'true',
    'agent',
    'Il final gate carica il frontend in un browser reale e pretende che le richieste della pagina arrivino a destinazione. Accerta cio'' che una probe HTTP lato server non puo'' vedere: CORS e URL costruiti a runtime. Il criterio nasce solo se il progetto ha un servizio frontend con porta allocata.',
    false
  ),
  (
    'agent.final_gate.browser_third_parties',
    'https://fonts.googleapis.com;https://fonts.gstatic.com;https://cdn.jsdelivr.net;https://unpkg.com',
    'agent',
    'Prefissi di URL esterni (CDN, font, telemetria) che non contano come difetto di integrazione: un font che non carica non e'' un''app rotta. Separatore '';'' perche'' un URL puo'' contenere virgole.',
    false
  ),
  (
    'agent.final_gate.browser_settle_ms',
    '2500',
    'agent',
    'Millisecondi di attesa che la rete della pagina si calmi dopo il primo render. Le chiamate dati partono dopo il montaggio dei componenti: osservare troppo presto vedrebbe una pagina che non ha ancora chiesto nulla, ed e'' per questo che sotto la soglia minima di richieste osservate il criterio dichiara NonConcludente invece di passare.',
    false
  )
ON CONFLICT (key) DO NOTHING;

-- Guard: le tre chiavi devono esistere, altrimenti il codice le leggerebbe dal
-- proprio default (criterio SPENTO) e la migrazione sarebbe passata a vuoto.
DO $$
DECLARE
  presenti INT;
BEGIN
  SELECT COUNT(*) INTO presenti
  FROM settings
  WHERE key IN (
    'agent.final_gate.browser_dialogue_enabled',
    'agent.final_gate.browser_third_parties',
    'agent.final_gate.browser_settle_ms'
  );
  IF presenti <> 3 THEN
    RAISE EXCEPTION 'mig 0681: attese 3 chiavi del dialogo browser, trovate %', presenti;
  END IF;
END $$;
