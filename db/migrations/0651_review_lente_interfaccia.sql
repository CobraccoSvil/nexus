-- 0651: la lente di interfaccia entra nel panel di review, con criteri scritti.
--
-- La figura UI (mig 0650) parla PRIMA che si costruisca. Questa migrazione le da'
-- il secondo tempo: rivedere cio' che e'' stato costruito e poterlo rimandare in
-- correzione. Senza, un'interfaccia inusabile passa comunque il gate: i revisori
-- generici leggono correttezza, sicurezza e regressioni, e un elenco senza stato
-- vuoto e'' corretto per tutti e tre.
--
-- CAUTELA, misurata. Il panel di review OGGI boccia su rilievi di UX minori
-- ("manca il loading state") mentre NON si accorgeva che l'app rispondeva 500 su
-- ogni scrittura. Un giudice di gusto senza criteri verificabili aumenta i
-- rimandi a vuoto, e i rimandi costano: un run del 27/07 ne ha fatti 3, per 2,1M
-- token e 3,08 USD. Per questo:
--
--   1. il prompt separa esplicitamente cio' che BLOCCA (verificabile leggendo il
--      codice) da cio' che e'' un suggerimento (preferenza estetica), e riprende
--      la calibrazione del contesto della mig 0633: i progetti qui sono prototipi
--      locali, non prodotti in vetrina;
--   2. la lente si convoca solo se i file MODIFICATI toccano l'interfaccia (fatto
--      del run, non intenzione del task);
--   3. non si SOMMA ai revisori: prende il posto di un generico, quindi il panel
--      costa quanto prima.
--
-- Idempotente: ON CONFLICT su tutte le tabelle.

-- ─────────────────────────────────────────────────────────────────────────────
-- (1) Il kind. Stessi tool del revisore generico (`review`, mig 0151) piu'' il
--     catalogo dei layout: un rilievo che cita il pattern violato e'' verificabile,
--     uno che dice "si vede male" no.
-- ─────────────────────────────────────────────────────────────────────────────
INSERT INTO nexus_subagent_definitions (kind, description, prompt_key, tool_whitelist, model_purpose, max_iterations, timeout_s, is_background) VALUES
    ('ui_reviewer',
     'Revisione post-implementazione con la lente dell''interfaccia: stati resi, esito delle azioni, raggiungibilita'', etichette. Output: review_verdict con findings verificabili.',
     'subagent.ui_reviewer.base',
     ARRAY['list_files','read_file','search_in_files','run_command','ui_layout_patterns'],
     'reviewer', 15, 240, false)
ON CONFLICT (kind) DO UPDATE SET
    description = EXCLUDED.description,
    prompt_key = EXCLUDED.prompt_key,
    tool_whitelist = EXCLUDED.tool_whitelist,
    model_purpose = EXCLUDED.model_purpose,
    max_iterations = EXCLUDED.max_iterations,
    timeout_s = EXCLUDED.timeout_s,
    updated_at = NOW();

-- ─────────────────────────────────────────────────────────────────────────────
-- (2) Il prompt. La sezione <cosa_blocca> e'' il contenuto della cautela: e''
--     l'elenco CHIUSO dei casi che possono valere un 'fail', e sono tutti
--     verificabili leggendo il codice.
-- ─────────────────────────────────────────────────────────────────────────────
INSERT INTO nexus_prompt_templates (key, category, title, content, is_active, version, updated_by, updated_at) VALUES
('subagent.ui_reviewer.base', 'automation', 'Review: lente interfaccia',
$$<role>Sei il revisore dell'INTERFACCIA nel panel di review di Nexus. Gli altri revisori guardano correttezza, sicurezza e regressioni; tu guardi cosa vede e cosa puo' fare chi usera' il risultato. Il tuo verdetto pesa quanto il loro: puo' rimandare il lavoro in correzione.</role>

<contesto>
Ricevi l'elenco dei file modificati dal run appena concluso. Rivedi SOLO quelle modifiche, non l'intero progetto. Il contesto d'uso: i progetti costruiti qui sono per default PROTOTIPI in sviluppo locale, salvo segnali espliciti del contrario (configurazione di deploy, dominio pubblico, README che dichiara la produzione).
</contesto>

<protocollo>
1. Leggi i file modificati. Se sono molti, parti da quelli di interfaccia (pagine, viste, componenti, fogli di stile).
2. Individua che tipo di schermata e' stata costruita e chiama ui_layout_patterns con l'app_type corrispondente: la scheda dice quali stati sono obbligatori per quel pattern. E' il tuo metro, e ti evita di giudicare a memoria.
3. Verifica nel codice, uno per uno, i casi elencati in <cosa_blocca>.
4. Chiudi con review_verdict.
</protocollo>

<cosa_blocca>
Questo e' l'elenco CHIUSO dei difetti che possono valere severity 'alta' (cioe' un rimando in correzione). Tutti si verificano LEGGENDO il codice, e ognuno richiede l'evidenza: quale file, quale punto.

- Uno stato obbligatorio del pattern non e' reso: non esiste ramo per il caricamento, per l'elenco vuoto, o per l'errore della chiamata. Un catch che ingoia l'errore senza mostrare nulla conta come stato mancante.
- Un'azione non ha esito visibile: dopo il salvataggio, l'eliminazione o l'invio, l'utente non riceve ne' conferma ne' errore.
- Un errore lascia l'interfaccia in uno stato che mente: il valore resta quello nuovo dopo un salvataggio fallito, la riga sparisce prima della conferma del server, l'indicatore di caricamento non si spegne mai.
- Un elemento necessario al compito non e' raggiungibile: non esiste percorso di navigazione per arrivarci, e' coperto, o e' reso solo fuori dallo schermo.
- Un campo di input senza etichetta associata, o un errore di validazione non collegato al campo che lo ha causato.
- Un'azione distruttiva senza conferma.
- La funzione richiesta non e' completabile dall'interfaccia prodotta.

NON e' mai 'alta', e NON puo' motivare un fail:
- colori, spaziature, tipografia, ombre, animazioni, arrotondamenti;
- "sarebbe piu' moderno", "meglio card che tabella", "userei un altro componente";
- differenze fra due soluzioni entrambe funzionanti;
- rifinitura di produzione assente in un prototipo locale (temi, adattamento a ogni dimensione di schermo, micro-interazioni), se il task non la chiedeva.
Questi vanno segnalati come severity 'bassa' o 'media', cioe' come suggerimenti.

In dubbio fra bloccante e suggerimento, scegli il suggerimento: un rimando a vuoto costa un ciclo intero di correzione, un suggerimento non costa nulla.
</cosa_blocca>

<anti_loop>
Un solo giro: file modificati, catalogo, verifica dei casi, verdetto. Non riaprire file gia' letti.
</anti_loop>

<output_format>
Chiudi chiamando review_verdict:
- verdict: pass se non hai trovato nulla di <cosa_blocca>; needs_changes se hai trovato difetti da correggere ma il lavoro regge; fail solo con almeno un difetto dell'elenco chiuso, documentato.
- findings: ognuno con file, severity ed evidenza concreta (cosa manca e dove). Cita la chiave del pattern quando il difetto e' uno stato obbligatorio non reso.
- summary: cosa hai verificato e cosa hai trovato, in prosa breve.
Un 'pass' di cortesia su un'interfaccia che non si puo' usare e' il peggior esito possibile; un 'fail' per una preferenza estetica e' il secondo peggiore.
</output_format>$$,
true, 1, 'system', NOW())
ON CONFLICT (key) DO UPDATE SET
    content = EXCLUDED.content,
    updated_at = NOW(),
    updated_by = 'migration_0651';

-- ─────────────────────────────────────────────────────────────────────────────
-- (3) Whitelist runtime dei kind.
-- ─────────────────────────────────────────────────────────────────────────────
UPDATE settings
   SET value = (
       SELECT string_agg(k, ',' ORDER BY k)
       FROM (
           SELECT DISTINCT trim(x) AS k
           FROM unnest(
               string_to_array(COALESCE(value, ''), ',') || ARRAY['ui_reviewer']
           ) AS x
           WHERE trim(x) <> ''
       ) t
   ),
       updated_at = NOW()
 WHERE key = 'orchestrator.subagent_kinds_whitelist';

-- ─────────────────────────────────────────────────────────────────────────────
-- (4) Quando convocarla: dai file MODIFICATI, non dal testo del task.
--     La regola di riconoscimento e'' il punto unico
--     nexus-agent-graph::decisions::ui_surface; qui vive solo il vocabolario.
-- ─────────────────────────────────────────────────────────────────────────────
INSERT INTO settings (key, value, category, description) VALUES
  ('orchestrator.review_ui_lens_enabled', 'true', 'orchestrator',
   'Il panel di review convoca la lente di interfaccia (kind ui_reviewer) quando i file modificati toccano una superficie di interfaccia. Non allarga il panel: la lente prende il posto di un revisore generico.'),
  ('orchestrator.review_ui_suffixes',
   '.tsx,.jsx,.vue,.svelte,.astro,.html,.htm,.css,.scss,.sass,.less,.styl',
   'orchestrator',
   'Suffissi di file che identificano una superficie di interfaccia. Confronto su fine-nome, case-insensitive.'),
  ('orchestrator.review_ui_path_segments',
   'components,component,pages,page,views,view,screens,screen,layouts,layout,ui,templates,partials,styles,css',
   'orchestrator',
   'Segmenti di percorso che identificano una superficie di interfaccia. Confronto come COMPONENTE INTERO del percorso: src/components/x.ts si, src/decomponents.ts no.')
ON CONFLICT (key) DO NOTHING;
