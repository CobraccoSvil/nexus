-- ADR 0017 v2 TODO 5 — aggiunge `imports` alla whitelist dei predicate
-- consentiti dal vincolo CHECK su `wiki_concept_triples` e su `wiki_links`.
--
-- Razionale: il code-graph reindex (vedi `projects::indexing::reindex_single_file`)
-- inserisce triple con predicate=`imports` quando un file di codice importa un
-- modulo / un altro file. Il CHECK originale (mig 0295) non lo includeva.

BEGIN;

-- ── wiki_concept_triples ─────────────────────────────────────────────────
ALTER TABLE wiki_concept_triples
    DROP CONSTRAINT IF EXISTS triple_predicate_check;
ALTER TABLE wiki_concept_triples
    ADD CONSTRAINT triple_predicate_check CHECK (predicate IN (
        'relates','supersedes','depends_on','illustrates','contradicts',
        'followup','correction_of','refines','duplicate_of',
        'blocks','blocked_by','mentions','implements','tests','imports'
    ));

-- ── wiki_concept_triples.source: aggiunge `static_analysis` ───────────────
-- Il code-graph reindex inserisce triple con source='static_analysis' (parser
-- regex degli import). La whitelist originale (mig 0295) accettava solo
-- wikilink|semantic|llm|user|agent|external.
ALTER TABLE wiki_concept_triples
    DROP CONSTRAINT IF EXISTS triple_source_check;
ALTER TABLE wiki_concept_triples
    ADD CONSTRAINT triple_source_check CHECK (source IN (
        'wikilink','semantic','llm','user','agent','external','static_analysis'
    ));

-- ── wiki_links (stesso vocabolario, mantenuto allineato) ──────────────────
ALTER TABLE wiki_links
    DROP CONSTRAINT IF EXISTS wiki_links_rel_type_check;
ALTER TABLE wiki_links
    ADD CONSTRAINT wiki_links_rel_type_check CHECK (rel_type IN (
        'relates','supersedes','depends_on','illustrates','contradicts',
        'followup','correction_of','refines','duplicate_of',
        'blocks','blocked_by','mentions','implements','tests','imports'
    ));

COMMIT;
