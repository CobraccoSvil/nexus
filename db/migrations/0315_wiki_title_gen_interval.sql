-- 0315_wiki_title_gen_interval.sql
-- Worker periodico di generazione titoli wiki (ADR 0017 v2): intervallo del loop.
-- Aggiunto da start_title_gen_worker (wiki/title_gen.rs). Senza questa chiave il
-- worker usa il safe_default 1800s; la esponiamo in settings per renderla
-- configurabile da DB (regola G, niente hardcoded come unica fonte).

INSERT INTO settings (key, value, category, description)
VALUES (
  'agent.wiki.title_gen_interval_secs',
  '1800',
  'agent',
  'Intervallo (secondi) del worker periodico che rigenera i titoli descrittivi dei doc wiki artefatto (chat_note/run_summary/other) per scope meta e tutti i progetti. Default 1800 (30 min).'
)
ON CONFLICT (key) DO NOTHING;
