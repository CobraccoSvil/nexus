-- 0642_catalogo_servizi_runtime_windows.sql
--
-- Allinea il catalogo `system.services_catalog` ai servizi che il runtime
-- Windows avvia davvero, perche' da qui in poi i manifest di servizio sono
-- DERIVATI dal catalogo (xtask service-manifests) invece che da una lista
-- scritta a mano fuori dal controllo di versione.
--
-- PERCHE' E' OBBLIGATORIA, non cosmetica. Il generatore emette un manifest per
-- ogni voce di catalogo che dichiara un `winsw_id`. Oggi qdrant non e' nel
-- catalogo e redis non ha `winsw_id`: derivare la lista dal catalogo SENZA
-- questa migrazione farebbe sparire i manifest di nexus-qdrant e nexus-garnet,
-- due processi che dev-start.ps1 avvia a ogni sessione. La migrazione precede
-- il generatore per la stessa ragione per cui la 0601 precedeva la rimozione
-- del crate billing: l'ordine non e' preferenza editoriale, e' il fix.
--
-- COSA NON FA. Non tocca i `systemd_unit` del catalogo, sette dei quali nominano
-- unit che sul disco non esistono (nexus-core-wsl, nexus-admin-wsl, ...).
-- Correggerli con una UPDATE sarebbe la toppa vietata dalla regola H: tornerebbero
-- a divergere alla prima unit rinominata, perche' nulla legherebbe il valore al
-- file. La convergenza del lato Unix e' un lavoro dichiarato a parte.
--
-- Idempotente e ri-eseguibile: dopo un wipe lo stato del catalogo e'
-- 0541 + 0601 + 0642, che e' esattamente cio' che il generatore si aspetta.

-- Precondizione esplicita: se il catalogo non esiste, questa migrazione non ha
-- un oggetto su cui lavorare e il silenzio sarebbe peggio dell'errore (la 0541
-- usa ON CONFLICT DO NOTHING, quindi un INSERT qui non aggiornerebbe nulla e
-- fallirebbe in silenzio).
DO $$
BEGIN
  IF NOT EXISTS (SELECT 1 FROM settings WHERE key = 'system.services_catalog') THEN
    RAISE EXCEPTION 'system.services_catalog assente: applicare prima la migrazione 0541';
  END IF;
END $$;

-- 1. Porta di qdrant come chiave settings (regola G: la porta si risolve dal DB).
--    Esiste gia' `qdrant_url` (mig 0002, 'http://localhost:6333'), che e' un URL:
--    ricavarne la porta con un parse di stringa sarebbe deduzione da testo
--    (regola M). Le due chiavi restano distinte e possono divergere: e' un debito
--    dichiarato, non un incidente. Il consolidamento (URL derivato dalla porta)
--    e' un lavoro a parte.
INSERT INTO settings (key, value, category, description) VALUES
  ('qdrant_port', '6333', 'infrastructure',
   'Porta HTTP del database vettoriale Qdrant. Fonte unica per il probe di stato del pannello Servizi e per il manifest di servizio generato.')
ON CONFLICT (key) DO NOTHING;

-- 2. Voce qdrant nel catalogo.
--    watchdog_managed=false di proposito: accendere l'auto-restart di un servizio
--    che finora non era ne' mostrato ne' sorvegliato e' una decisione separata,
--    da prendere dopo aver misurato come si comporta. Qui si dichiara solo che
--    esiste e come si chiama il suo manifest.
UPDATE settings
SET value = (
      (value::jsonb || jsonb_build_array(jsonb_build_object(
        'name',             'qdrant',
        'label',            'Qdrant',
        'port_setting_key', 'qdrant_port',
        'description',      'Database vettoriale (RAG)',
        'readonly',         false,
        'controllable',     true,
        'panel_shown',      true,
        'watchdog_managed', false,
        'winsw_id',         'nexus-qdrant'
      )))::text
    ),
    updated_at = NOW()
WHERE key = 'system.services_catalog'
  AND NOT (value::jsonb @> '[{"name": "qdrant"}]'::jsonb);

-- 3. redis: aggiunta del solo `winsw_id`.
--    Su Windows l'implementazione in ascolto sulla 6379 e' Garnet, il cui
--    servizio si chiama nexus-garnet; su Unix e' il container dichiarato in
--    docker_container, che resta invariato (e' un hint di provenienza, non e'
--    usato per lo stato). Label e description NON si toccano: sono visibili nel
--    pannello e cambiarle non serve a generare il manifest.
UPDATE settings
SET value = (
      SELECT COALESCE(jsonb_agg(
               CASE WHEN elem->>'name' = 'redis'
                    THEN elem || '{"winsw_id": "nexus-garnet"}'::jsonb
                    ELSE elem END
               ORDER BY ord
             ), '[]'::jsonb)::text
      FROM jsonb_array_elements(value::jsonb) WITH ORDINALITY AS t(elem, ord)
    ),
    updated_at = NOW()
WHERE key = 'system.services_catalog'
  AND value::jsonb @> '[{"name": "redis"}]'::jsonb
  AND NOT (value::jsonb @> '[{"name": "redis", "winsw_id": "nexus-garnet"}]'::jsonb);
