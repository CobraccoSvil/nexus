-- 0629_enable_gate_probe_required.sql
--
-- CAUSA RADICE (regola H, "fix del sorgente"): is_enabled diventa true per SEI vie,
-- ma solo DUE verificano l'esistenza reale al provider via probe
-- (write_probe_healthy_flags, reenable_candidate). Le altre quattro abilitano SENZA
-- chiamare il provider: auto_upgrade_models_and_routing (per FAMIGLIA, allowlist
-- regex), reconcile_enable_returning_to_policy, do_reenable_model, e le seed-migration
-- con UPDATE is_enabled=true diretto. E la base table nasce is_enabled DEFAULT TRUE
-- (0006). Cosi' un nome plausibile-ma-inesistente (claude-sonnet-4-6) che matcha
-- l'allowlist e ha un prezzo diventa enabled senza mai essere provato -> 400 in
-- produzione. Viola regola L: l'enable ha 6 punti, 4 senza verifica.
--
-- FIX (punto unico vero, come 0583 per pricing_state): un TRIGGER BEFORE INSERT/UPDATE
-- e' l'UNICO choke-point attraversato da TUTTI gli scrittori (Rust + migrazioni +
-- discovery + UPSERT LiteLLM). Richiede una PROVA d'inference reale
-- (last_probe_healthy_at, scritto SOLO dai due enable verificati nella stessa
-- statement) per consentire la transizione a is_enabled=true. Nessuna via — inclusa
-- una futura seed-migration o il DEFAULT TRUE — puo' piu' abilitare un modello mai
-- provato. Rende superflue le toppe manuali 0556/0187/0628.
--
-- SICUREZZA (verificato in transazione rollback sul catalog vivo): il trigger fire
-- SOLO sulla TRANSIZIONE verso enabled (INSERT con true, o UPDATE false->true). Una
-- riga GIA' enabled aggiornata per altri motivi (is_enabled resta true) NON viene
-- toccata: nessuna decimazione del catalog esistente. Il ripristino automatico di un
-- modello sano avviene via reenable_candidate (probe reale -> scrive il timestamp),
-- sopravvive a riavvio/redeploy senza intervento manuale (regola H).

-- Prova d'esistenza reale: NULL = mai provato con successo. Scritto solo dai due
-- enable verificati (write_probe_healthy_flags, reenable_candidate).
ALTER TABLE ai_price_catalog
  ADD COLUMN IF NOT EXISTS last_probe_healthy_at TIMESTAMPTZ;

-- Grandfathering: i modelli ATTUALMENTE enabled sono presunti verificati (sono in
-- uso e rispondono). Backfill del timestamp cosi' che, se in futuro vengono
-- disabilitati e poi re-inclusi in policy (reconcile_enable_returning_to_policy) o
-- ricompaiono in API (do_reenable_model), il gate NON li blocchi (evita la
-- regressione "modello reale che rientra in policy resta spento"). I ghost gia'
-- disabilitati da 0628 NON sono enabled -> restano a NULL -> il gate li blocca.
-- Idempotente (WHERE is_enabled). NB: eseguito PRIMA di creare il trigger, e
-- comunque non lo attiverebbe (nessuna transizione: is_enabled resta true).
UPDATE ai_price_catalog
   SET last_probe_healthy_at = COALESCE(last_probe_healthy_at, NOW())
 WHERE is_enabled = true;

CREATE OR REPLACE FUNCTION ai_price_catalog_enforce_probe_before_enable()
RETURNS TRIGGER AS $$
BEGIN
  -- Fire SOLO su una transizione VERSO enabled: INSERT con true, oppure
  -- UPDATE da false a true. Le righe gia' enabled (OLD.is_enabled true) non
  -- transitano -> non vengono toccate (nessuna decimazione del catalog).
  IF NEW.is_enabled
     AND (TG_OP = 'INSERT' OR NOT OLD.is_enabled) THEN
    -- Serve una prova d'inference reale scritta nella STESSA statement dai due
    -- enable verificati. Assente -> resta disabilitato con reason parlante.
    IF NEW.last_probe_healthy_at IS NULL THEN
      NEW.is_enabled := false;
      NEW.auto_disabled_at := COALESCE(NEW.auto_disabled_at, NOW());
      NEW.auto_disabled_reason := 'unverified_no_probe';
    END IF;
  END IF;
  RETURN NEW;
END;
$$ LANGUAGE plpgsql;

DROP TRIGGER IF EXISTS trg_ai_price_catalog_enable_gate ON ai_price_catalog;
CREATE TRIGGER trg_ai_price_catalog_enable_gate
  BEFORE INSERT OR UPDATE ON ai_price_catalog
  FOR EACH ROW
  EXECUTE FUNCTION ai_price_catalog_enforce_probe_before_enable();
