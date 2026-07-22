-- 0634_media_usage_units.sql
--
-- CAUSA: le 4 modalita' non-testuali del gateway (image-gen, video-gen,
-- transcribe, TTS) non scrivono NULLA in `ai_usage_ledger`. Non e' una
-- dimenticanza: i doc-comment a routes.rs:1295/1442/1583/1727 dichiarano la
-- scelta e ne danno la ragione — `record_usage_to_ledger` e' per-token, mentre
-- queste modalita' si pagano a immagine, a secondo o a carattere, e scrivere una
-- riga senza il prezzo giusto avrebbe significato inventare un costo (regola
-- G/H). Il blocco non era nell'handler: era nello SCHEMA, che non ha alcun modo
-- di dire "3 immagini" o "42 secondi".
--
-- Conseguenza pratica: consumo reale, denaro reale, zero righe. Quote e report
-- costi sottostimano in modo sistematico e silenzioso.
--
-- Questa migrazione da' allo schema il vocabolario che gli manca. NON inventa
-- prezzi: il listino per-unita' nasce vuoto, e finche' resta vuoto le righe
-- media portano `total_cost = 0` con `details.price_state = 'not_in_catalog'`,
-- esattamente come gia' accade per un modello testuale non a listino. Il
-- CONSUMO pero' smette di essere invisibile, ed e' attribuibile a utente e
-- progetto. Popolando `ai_price_catalog_unit` i costi si accendono da soli,
-- senza toccare il codice (regola G).

-- ── Parte A — vocabolario del consumo su ai_usage_ledger ────────────────────
--
-- Quattro colonne, tutte con DEFAULT o NULL: nessun backfill, nessun UPDATE
-- sulle righe esistenti, e i cinque INSERT/UPDATE gia' in giro continuano a
-- funzionare senza elencarle.
--
-- Perche' `(quantity, quantity_unit)` generico e non quattro colonne
-- `images_count / video_seconds / audio_seconds / characters`: una colonna per
-- modalita' significa una migrazione per ogni modalita' futura (regola L).
-- `usage_kind` e' il discriminatore STRUTTURATO che oggi manca del tutto: senza
-- di esso l'unico modo di sapere che una riga e' un'immagine sarebbe leggere
-- `details->>'feature'`, che e' testo libero scritto dal chiamante — cioe'
-- dedurre lo stato tecnico dalla prosa, vietato dalla regola M.
ALTER TABLE ai_usage_ledger
    ADD COLUMN IF NOT EXISTS usage_kind      TEXT NOT NULL DEFAULT 'text',
    ADD COLUMN IF NOT EXISTS quantity        NUMERIC(18,6),
    ADD COLUMN IF NOT EXISTS quantity_unit   TEXT,
    ADD COLUMN IF NOT EXISTS quantity_source TEXT NOT NULL DEFAULT 'none';

COMMENT ON COLUMN ai_usage_ledger.usage_kind IS
    'Modalita della chiamata: text (default, tutto lo storico) | image | video | audio_in | audio_out. Discriminatore strutturato: non dedurre la modalita da details->>''feature'' (regola M).';
COMMENT ON COLUMN ai_usage_ledger.quantity IS
    'Quantita consumata nell unita di quantity_unit (es. 3 immagini, 42 secondi). NULL = non conoscibile, mai 0 di ripiego.';
COMMENT ON COLUMN ai_usage_ledger.quantity_unit IS
    'Unita di quantity: image | second | character. NULL per le righe a token, che usano prompt/completion_tokens.';
COMMENT ON COLUMN ai_usage_ledger.quantity_source IS
    'Da dove viene quantity: provider (dichiarata nella risposta) | request (dedotta da cio che abbiamo chiesto) | none (ignota). Serve a non spacciare una stima per un dato misurato.';

-- `quantity_source` e' la colonna che sembra superflua e non lo e': nessuno dei
-- quattro provider dichiara oggi la quantita' prodotta (OpenAI Images scarta
-- l'usage, la trascrizione gira con response_format=json che non porta la
-- durata, il video non riporta i secondi effettivi). Fatturare cio' che abbiamo
-- CHIESTO invece di cio' che e' stato prodotto e' accettabile, ma dev'essere
-- scritto nella riga: altrimenti fra sei mesi nessuno sa se quel 42 e' misurato
-- o presunto.
ALTER TABLE ai_usage_ledger
    ADD CONSTRAINT chk_ledger_usage_kind
        CHECK (usage_kind IN ('text', 'image', 'video', 'audio_in', 'audio_out')),
    ADD CONSTRAINT chk_ledger_quantity_unit
        CHECK (quantity_unit IS NULL OR quantity_unit IN ('image', 'second', 'character')),
    ADD CONSTRAINT chk_ledger_quantity_source
        CHECK (quantity_source IN ('provider', 'request', 'none')),
    -- "non lo so" non si traveste da zero, e un numero non resta senza provenienza.
    ADD CONSTRAINT chk_ledger_quantity_coerente
        CHECK ((quantity_source = 'none') = (quantity IS NULL));

-- Le righe media restano `status='finalized'` come quelle del gateway: NON si
-- tocca il CHECK su status. Uno status nuovo (es. 'metered') sarebbe rifiutato
-- dal vincolo di 0006 e, se anche lo si estendesse, renderebbe la riga invisibile
-- a tutti i lettori esistenti, che filtrano su 'finalized'.

CREATE INDEX IF NOT EXISTS idx_ledger_usage_kind
    ON ai_usage_ledger (usage_kind)
    WHERE usage_kind <> 'text';

-- ── Parte B — listino per-unita', in una tabella SEPARATA ───────────────────
--
-- NON righe in `ai_price_catalog`, e la ragione e' un bug che sarebbe passato
-- inosservato: quella tabella non ha alcun UNIQUE su (provider, model, currency)
-- e il lookup del punto unico (nexus-pricing) fa
-- `ORDER BY effective_from DESC LIMIT 1`. Un modello fatturato SIA a token SIA a
-- immagine — gpt-image-1 e' esattamente questo caso — avrebbe due righe per la
-- stessa chiave, e la riga per-immagine potrebbe vincere il LIMIT 1 facendo
-- prezzare la CHAT col costo per-immagine. In silenzio, senza che nulla
-- fallisca. E' lo stesso difetto della mig 0477 (un valore con due significati),
-- spostato dalla colonna alla riga.
CREATE TABLE IF NOT EXISTS ai_price_catalog_unit (
    id             UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    provider       TEXT NOT NULL,
    model          TEXT NOT NULL,
    -- L'unita' fa parte della CHIAVE: lo stesso modello puo' avere un prezzo
    -- per immagine e uno per secondo senza che si sovrascrivano.
    unit           TEXT NOT NULL,
    unit_cost      NUMERIC(18,6) NOT NULL,
    currency       TEXT NOT NULL,
    effective_from TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    effective_to   TIMESTAMPTZ,
    source         TEXT NOT NULL DEFAULT 'manual',
    created_at     TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    CONSTRAINT chk_price_unit_kind
        CHECK (unit IN ('image', 'second', 'character')),
    CONSTRAINT chk_price_unit_cost_non_negativo
        CHECK (unit_cost >= 0)
);

-- L'unicita' che manca all'altra tabella, qui c'e' per costruzione: una sola
-- riga viva per (provider, model, unit, currency).
CREATE UNIQUE INDEX IF NOT EXISTS uq_price_unit_vivo
    ON ai_price_catalog_unit (provider, model, unit, currency)
    WHERE effective_to IS NULL;

CREATE INDEX IF NOT EXISTS idx_price_unit_lookup
    ON ai_price_catalog_unit (provider, model, unit, currency, effective_from DESC);

COMMENT ON TABLE ai_price_catalog_unit IS
    'Listino per unita non-token (immagine, secondo, carattere). Separato da ai_price_catalog perche quella tabella non ha UNIQUE su (provider,model,currency) e il lookup fa ORDER BY effective_from DESC LIMIT 1: una riga per-immagine potrebbe vincere sul lookup a token e prezzare la chat col costo sbagliato. Nasce VUOTA di proposito: finche non e popolata le righe media hanno costo 0 dichiarato (price_state=not_in_catalog), mai un prezzo inventato.';
COMMENT ON COLUMN ai_price_catalog_unit.unit_cost IS
    'Costo di UNA unita nella valuta indicata (non per milione, a differenza di ai_price_catalog).';
