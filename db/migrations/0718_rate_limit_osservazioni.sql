-- 0718_rate_limit_osservazioni.sql
--
-- Telemetria degli header di rate limit dichiarati dai fornitori sulle
-- risposte HTTP. SOLO sensore: nessun consumatore decide su queste righe
-- (collegare il registro a cooldown/selezione e' fase futura, e va fatto nel
-- punto unico della portata del cooldown, non qui). Scrittore unico: il
-- flusher del gateway (rate_limit_headers.rs), che persiste a intervallo le
-- sole voci cambiate dall'ultimo giro.
--
-- La chiave e' (provider, model) perche' i bucket di groq/openai sono per
-- MODELLO: una colonna su nexus_provider_health (per fornitore) collasserebbe
-- bucket diversi, e quella tabella ha gia' il suo scrittore (il probe di
-- mcp-core) — un secondo scrittore dal gateway rifarebbe il conflitto
-- "cooldown cancellato da chi misura un'altra cosa".
--
-- Le semantiche NON sono uniformi fra fornitori (groq: limit-requests e' AL
-- GIORNO, limit-tokens AL MINUTO — MISURATO 13/08/2026): i numeri si leggono
-- col campo raw accanto, che conserva gli header originali cosi' come il wire
-- li ha detti (regola M: si conserva la fonte, non una nostra interpretazione).

CREATE TABLE IF NOT EXISTS nexus_rate_limit_observations (
    provider            text NOT NULL,
    -- Modello della richiesta che ha osservato gli header. '' riservato a
    -- osservazioni senza modello (oggi nessun produttore le emette).
    model               text NOT NULL DEFAULT '',
    requests_limit      bigint NULL,
    requests_remaining  bigint NULL,
    requests_reset_at   timestamptz NULL,
    tokens_limit        bigint NULL,
    tokens_remaining    bigint NULL,
    tokens_reset_at     timestamptz NULL,
    -- Header originali nome->valore: l'audit e la fonte da cui rileggere cio'
    -- che la normalizzazione qui sopra non copre (es. la famiglia
    -- output-tokens di anthropic).
    raw                 jsonb NOT NULL,
    observed_at         timestamptz NOT NULL,
    PRIMARY KEY (provider, model)
);

COMMENT ON TABLE nexus_rate_limit_observations IS
    'Ultima osservazione degli header di rate limit per coppia (provider, model). Solo sensore (mig 0718): scrittore unico il flusher del gateway, nessun consumatore decisionale in questa fase.';

INSERT INTO settings (key, value, category, description) VALUES
  ('gateway.rate_limit_snapshot_interval_s', '30', 'gateway',
   'Ogni quanti secondi il gateway persiste le osservazioni di rate limit cambiate. 0 = snapshot disattivo (la lettura in-memory resta).')
ON CONFLICT (key) DO NOTHING;
