-- Mig 0195 — Robustezza pipeline allegati (FIX 1-4 ADR 0012).
--
-- 4 fix strutturali al gap rilevato nel test E2E con Figma (canvas.fig dentro
-- PL.make): inspector con next_action_recommended, cache letture, pre-extract
-- automatica, cap budget letture per sessione.
--
-- Tutti i parametri restano configurabili da DB: nessun hardcoded fallback
-- nel codice. Idempotente.

INSERT INTO settings (key, value, category, description, updated_at)
VALUES
    (
        'agent.attachment.preextract_enabled',
        'true',
        'agent',
        'Pre-extraction automatica del contenuto strutturato di PDF/DOCX/Figma allegati. Default true. Disattivare se causa latenza eccessiva all''invio del primo messaggio.',
        NOW()
    ),
    (
        'agent.attachment.preextract_max_chars',
        '50000',
        'agent',
        'Limite totale (in caratteri) del contenuto pre-extracted sommando tutti gli allegati del turno. Se eccede, gli ultimi allegati non vengono pre-extracted.',
        NOW()
    ),
    (
        'agent.attachment.session_read_budget_bytes',
        '500000',
        'agent',
        'Cap cumulativo (byte) delle letture nexus_read_attachment + nexus_read_archive_entry per sessione. Oltre la soglia, il brain risponde con tool_result sintetico che invita a usare gli estrattori strutturati.',
        NOW()
    ),
    (
        'agent.attachment.read_cache_ttl_seconds',
        '300',
        'agent',
        'TTL (secondi) della cache LRU read_cache che deduplica chiamate identiche a nexus_read_attachment / nexus_read_archive_entry. Default 5 minuti.',
        NOW()
    )
ON CONFLICT (key) DO NOTHING;
