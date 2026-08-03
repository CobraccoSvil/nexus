-- 0673_prompt_porta_senza_default_numerico.sql
--
-- Il prompt vietava il default numerico e poi lo autorizzava, a una condizione
-- che il file non puo' conservare: «Se serve un default, usa la porta ALLOCATA
-- da request_port». "Allocata" e' una proprieta' del MOMENTO in cui si scrive:
-- il sorgente la incide e non ha modo di sapere che e' scaduta.
--
-- MISURATO il 03/08/2026 su agenda-corsi: `frontend/vite.config.ts:6` =
-- `Number(env.PORT) || Number(process.env.PORT) || 26548` — la forma vietata
-- dalla prima meta' della frase, resa lecita dalla seconda. La porta 26548 nel
-- frattempo risultava registrata a un ALTRO servizio, e il frontend che ci
-- ripiegava non aveva modo di accorgersene.
--
-- Il rimedio non e' un divieto piu' severo: e' togliere la condizione. Un
-- servizio avviato senza PORT deve FALLIRE rumorosamente, perche' quel
-- fallimento e' l'unico modo per accorgersi che un percorso di avvio non ha
-- iniettato la porta — ed e' la stessa scelta appena fatta nel codice, dove lo
-- start di un web service da un contesto privo di registro viene rifiutato
-- invece di procedere in silenzio (service_manager.rs).
--
-- Si toglie anche la dipendenza `fe_pkg <- fe_port` dal piano esemplare del
-- planner, che insegnava esattamente il travaso dal registro ai file.

UPDATE nexus_prompt_templates SET content = replace(content,
  '- Un fallback env con default numerico (process.env.PORT || 5000, os.environ.get("PORT", 5000), env::var("PORT").unwrap_or("3000")) E'' a tutti gli effetti una porta hardcoded: viene RIFIUTATO in scrittura. Se serve un default, usa la porta ALLOCATA da request_port.',
  '- Un fallback env con default numerico (process.env.PORT || 5000, os.environ.get("PORT", 5000), env::var("PORT").unwrap_or("3000")) E'' a tutti gli effetti una porta hardcoded: viene RIFIUTATO in scrittura. NON esiste un numero ammesso al suo posto, nemmeno una porta appena allocata: "allocata" vale nel momento in cui scrivi, e il file non sa quando smette di valere. Se PORT non e'' definita il servizio deve USCIRE CON ERRORE, non ripiegare: e'' cosi'' che ti accorgi di un avvio che non te l''ha iniettata, invece di finire su una porta di qualcun altro.')
WHERE key = 'system.nexus_base';

-- Il piano esemplare non deve piu' far dipendere la scrittura dei file di
-- configurazione dal task di allocazione: e' il travaso, insegnato per esempio.
UPDATE nexus_prompt_templates SET content = replace(content,
  '9. [pending] (fe_pkg <- fe_port) Scrivi frontend/package.json + Vite config',
  '9. [pending] (fe_pkg) Scrivi frontend/package.json + Vite config (la porta si legge da process.env.PORT, mai incisa)')
WHERE key = 'agent.planner.base';

UPDATE nexus_prompt_templates SET content = replace(content,
  '4. [pending] (be_pkg <- be_port,db) Scrivi backend/package.json + backend/.env',
  '4. [pending] (be_pkg <- db) Scrivi backend/package.json (la porta la inietta Nexus in PORT; il .env non la contiene)')
WHERE key = 'agent.planner.base';

-- Guard: nessun template deve tornare ad autorizzare un numero di porta come
-- default nei sorgenti. Il pattern e' la CONDIZIONE, non la parola "porta":
-- e' la clausola «se serve un default, usa la porta allocata» che riapriva la
-- strada al literal.
DO $$
DECLARE
    autorizzano bigint;
BEGIN
    SELECT count(*) INTO autorizzano
    FROM nexus_prompt_templates
    WHERE content ILIKE '%se serve un default, usa la porta allocata%'
       OR content ILIKE '%usa la porta ALLOCATA da request_port.%';
    IF autorizzano > 0 THEN
        RAISE EXCEPTION
            'nexus_prompt_templates: % template autorizzano ancora un default numerico di porta',
            autorizzano;
    END IF;
END $$;
