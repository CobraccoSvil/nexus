-- 0650: la lente che guarda l'interfaccia, e il catalogo che le da' qualcosa da citare.
--
-- CAUSA. Le sei figure del Consiglio delle Competenze (mig 0546) sono tutte lenti
-- di processo, architettura e rischio: programma, progetto, requisiti, design del
-- codice, infrastruttura, sicurezza. NESSUNA guarda l'interfaccia. Il 2026-07-28,
-- sul progetto gestione-spese, il consiglio ha deliberato "procede con modifiche"
-- e l'app e' uscita con pagine che montano i componenti senza gerarchia visiva,
-- senza stati di caricamento e senza stato vuoto. Non era un parere sbagliato:
-- nessuno aveva il compito di guardarla.
--
-- La figura da sola non basta. Un parere che dice "servirebbe un layout migliore"
-- e' un aggettivo, non un progetto: chi implementa non sa cosa fare di diverso.
-- Per questo la figura arriva insieme a un CATALOGO di layout di riferimento, che
-- il parere puo' CITARE e l'esecuzione applicare.
--
-- Perche' una tabella e non la Knowledge Base: `knowledge_search` interroga
-- `wiki_docs` con `scope = 'project' AND project_id = <progetto corrente>`. Un
-- catalogo trasversale messo li' andrebbe seedato in OGNI progetto: N copie dello
-- stesso testo che divergono alla prima correzione (regola L).
--
-- Idempotente: ON CONFLICT su tutte le tabelle, CREATE TABLE IF NOT EXISTS.

-- ─────────────────────────────────────────────────────────────────────────────
-- (1) Catalogo dei pattern di layout. Le colonne SONO il contratto: chi aggiunge
--     un pattern compila queste sezioni, non un testo libero. `required_states`
--     e' la parte verificabile — cio' su cui un rilievo puo' essere bloccante
--     senza diventare una preferenza estetica.
-- ─────────────────────────────────────────────────────────────────────────────
CREATE TABLE IF NOT EXISTS nexus_ui_layout_patterns (
    key             TEXT PRIMARY KEY,
    app_type        TEXT NOT NULL,
    title           TEXT NOT NULL,
    when_to_use     TEXT NOT NULL,
    structure       TEXT NOT NULL,
    required_states TEXT NOT NULL,
    anti_patterns   TEXT NOT NULL,
    is_active       BOOLEAN NOT NULL DEFAULT TRUE,
    created_at      TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    updated_at      TIMESTAMPTZ NOT NULL DEFAULT NOW()
);

COMMENT ON TABLE nexus_ui_layout_patterns IS
'Catalogo dei layout di riferimento citabili dal consigliere UI/UX e applicabili
dall''esecuzione. Letto dal punto unico nexus-agent-tools::ui_patterns::load_patterns
(tool read-only ui_layout_patterns). Aggiungere un pattern e'' una riga, non un deploy.';

CREATE INDEX IF NOT EXISTS idx_ui_layout_patterns_app_type
    ON nexus_ui_layout_patterns (app_type) WHERE is_active;

INSERT INTO nexus_ui_layout_patterns
    (key, app_type, title, when_to_use, structure, required_states, anti_patterns) VALUES

('crud_lista_form_dettaglio', 'crud', 'CRUD: lista, form, dettaglio',
 'Gestione di un insieme di record omogenei che l''utente crea, cerca, modifica ed elimina: spese, clienti, ordini, task, prodotti. E'' il pattern piu'' frequente; se la richiesta dice "gestione di X", parti da qui.',
 'Tre schermate collegate, non tre pagine scollegate.
LISTA (la casa): titolo della sezione + azione primaria "Nuovo" in alto a destra, sempre visibile; sotto, la barra di ricerca/filtri; poi la tabella o le card. Ogni riga porta le 3-5 colonne che servono a RICONOSCERE il record (non tutti i campi) e le azioni di riga a destra. In fondo, la paginazione se i record possono superare la prima schermata.
FORM (creazione e modifica sono la STESSA schermata, cambia solo il titolo e il valore iniziale dei campi): campi in colonna singola, raggruppati per significato, con le etichette sopra i campi; i campi obbligatori marcati; in fondo, a destra, "Annulla" (secondario) e "Salva" (primario), in quest''ordine.
DETTAGLIO (solo se il record ha piu'' informazioni di quante ne mostri la lista): intestazione con l''identificativo del record e le azioni, poi i dati raggruppati, poi le entita'' collegate.
GERARCHIA: in ogni schermata deve esserci UNA sola azione primaria evidenziata. Il resto e'' secondario.',
 'La lista ha tre stati oltre a quello pieno, e vanno resi tutti:
- CARICAMENTO: scheletro delle righe o indicatore nell''area della tabella (mai una pagina bianca, mai un salto di layout quando arrivano i dati);
- VUOTO senza filtri attivi (nessun record esiste): messaggio che spiega cosa comparira'' qui + l''azione per creare il primo record. E'' il primo schermo che un utente nuovo vede;
- VUOTO con filtri attivi (nessun risultato): messaggio diverso dal precedente, che dice che i filtri non danno risultati + azione per azzerarli;
- ERRORE di caricamento: messaggio leggibile + pulsante per riprovare. Mai una lista vuota che finge che non ci siano dati.
Il form: stato di invio in corso (pulsante disabilitato e in attesa, per non salvare due volte), errori di validazione ACCANTO al campo che li ha causati, errore di salvataggio visibile senza perdere quanto digitato.
L''eliminazione chiede conferma e dice cosa sta per essere eliminato.',
 'NON mettere tutti i campi del record come colonne della lista: la riga diventa illeggibile e non si distingue un record dall''altro.
NON usare una pagina bianca durante il caricamento.
NON lasciare la lista vuota senza spiegazione: e'' lo schermo che l''utente vede per primo, ed e'' quello che decide se ha capito l''app.
NON mostrare gli errori di validazione solo in cima al form: vanno accanto al campo.
NON eliminare senza conferma, e NON far sparire la riga prima che il server abbia confermato.
NON aprire il form in una finestra modale se ha piu'' di 6-7 campi: serve una schermata.'),

('dashboard_panoramica', 'dashboard', 'Dashboard: panoramica in un colpo d''occhio',
 'La schermata iniziale che risponde a "come sta andando?" con numeri e andamenti. Va usata quando l''utente deve CAPIRE lo stato, non modificarlo. Se serve a modificare dati, il pattern giusto e'' un altro.',
 'Densita'' decrescente dall''alto verso il basso.
FASCIA 1 - gli indicatori chiave: da 3 a 5 numeri, non di piu''. Ognuno con etichetta, valore grande e leggibile, e il confronto che gli da'' senso (variazione rispetto al periodo precedente). Un numero senza confronto non informa.
FASCIA 2 - gli andamenti: 1 o 2 grafici, ciascuno con un titolo che dice cosa mostra e in che periodo. Un grafico senza unita'' di misura e senza periodo e'' decorazione.
FASCIA 3 - il dettaglio: l''elenco degli elementi recenti o critici, con il collegamento alla schermata che li gestisce.
Il selettore del periodo, se esiste, sta in alto e vale per TUTTA la schermata: mai un periodo diverso per riquadro senza dirlo.',
 'Ogni riquadro ha i propri stati, perche'' i dati arrivano da fonti diverse e non insieme:
- CARICAMENTO per riquadro (scheletro delle dimensioni finali, cosi'' la pagina non salta quando i dati arrivano);
- VUOTO: "nessun dato nel periodo selezionato" dentro il riquadro, non un riquadro assente;
- ERRORE per riquadro: il riquadro dice che non ha potuto caricare e offre di riprovare, mentre gli altri continuano a funzionare. Un riquadro rotto non deve portarsi via la schermata;
- ZERO come valore legittimo: "0" e'' un dato, e va distinto dall''assenza di dato.',
 'NON mettere piu'' di 5 indicatori nella prima fascia: se sono tutti importanti, nessuno lo e''.
NON mostrare un numero senza il suo confronto o il suo periodo.
NON far saltare il layout quando i dati arrivano: lo spazio dei riquadri si riserva prima.
NON far cadere l''intera dashboard perche'' una fonte non risponde.
NON usare un grafico dove basta un numero.'),

('wizard_multi_step', 'wizard', 'Procedura guidata a passi',
 'Un compito lungo che ha senso solo completo e che si puo'' spezzare in passi con un ordine obbligato: primo avvio, configurazione iniziale, richiesta articolata, pagamento. Se i campi sono meno di 8 e non hanno dipendenze fra loro, NON serve un wizard: serve un form.',
 'L''utente deve sempre sapere dove si trova, quanto manca e come tornare indietro.
INTESTAZIONE FISSA: indicatore dei passi con il passo corrente evidenziato, i passi completati distinguibili da quelli futuri, e il numero totale. "Passo 2 di 4" e'' meglio di una barra senza numeri.
CORPO: un solo obiettivo per passo. Se un passo ha piu'' di 8-10 campi, e'' due passi.
PIEDE FISSO: "Indietro" a sinistra, "Avanti"/"Conferma" a destra. L''ultimo passo si chiama "Conferma" e mostra il RIEPILOGO di cio'' che e'' stato inserito, con il collegamento per correggere ogni sezione senza ricominciare.
Dopo la conferma: schermata di esito, con cosa e'' successo e qual e'' il passo successivo.',
 '- VALIDAZIONE al passaggio: si convalida uscendo dal passo, non alla fine. Un errore scoperto al quarto passo su un dato del primo e'' un lavoro perso;
- INVIO IN CORSO: pulsante di conferma disabilitato e in attesa, perche'' un doppio invio crea due record;
- ERRORE DI INVIO: il wizard resta dov''e'', con i dati INTATTI e il messaggio dell''errore. Mai riportare al primo passo;
- ABBANDONO: se l''utente esce a meta'', va avvertito che perdera'' i dati (o vanno conservati, dichiarandolo);
- ESITO: la schermata finale dice esplicitamente se e'' andata bene o male. Il silenzio dopo la conferma e'' il difetto piu'' comune di questo pattern.',
 'NON usare un wizard per un form corto.
NON convalidare solo alla fine.
NON perdere i dati quando l''invio fallisce.
NON nascondere quanti passi restano.
NON impedire di tornare indietro a rivedere cio'' che si e'' inserito.
NON far finire la procedura senza una schermata di esito.'),

('master_detail', 'master_detail', 'Elenco a sinistra, dettaglio a destra',
 'Consultazione e modifica alternate su molti elementi, quando l''utente passa spesso dall''uno all''altro: messaggi, conversazioni, file, impostazioni con molte sezioni, registri. Evita il viavai lista-dettaglio-lista.',
 'Due colonne su schermo largo, due schermate su schermo stretto.
COLONNA SINISTRA (elenco): ricerca in cima, poi gli elementi, ciascuno con l''informazione che serve a sceglierlo (titolo + un dato di contesto, es. la data). L''elemento selezionato resta EVIDENZIATO finche'' e'' aperto: senza questo l''utente perde il segno.
COLONNA DESTRA (dettaglio): intestazione con il titolo dell''elemento e le sue azioni, poi il contenuto. La colonna scorre in modo indipendente dall''elenco.
SU SCHERMO STRETTO: l''elenco e'' la schermata; toccando un elemento si va al dettaglio, che deve avere un "Indietro" esplicito verso l''elenco. Non affiancare mai due colonne su un telefono.',
 '- NESSUNA SELEZIONE (primo ingresso): la colonna di destra dice cosa fare ("seleziona un elemento per vederne il dettaglio"), non resta bianca;
- CARICAMENTO delle due colonne indipendente: l''elenco puo'' essere pronto mentre il dettaglio carica;
- VUOTO dell''elenco: come nel pattern CRUD, distinguendo "non c''e'' nulla" da "la ricerca non trova nulla";
- ERRORE del dettaglio: resta nella colonna di destra, con la possibilita'' di riprovare, mentre l''elenco continua a funzionare;
- ELEMENTO ELIMINATO mentre era aperto: il dettaglio torna allo stato "nessuna selezione", dicendolo.',
 'NON perdere l''evidenziazione dell''elemento selezionato.
NON lasciare bianca la colonna di destra al primo ingresso.
NON far scorrere insieme le due colonne.
NON affiancare le due colonne su schermo stretto.
NON ricaricare l''elenco a ogni selezione.'),

('impostazioni_sezioni', 'settings', 'Impostazioni raggruppate per sezioni',
 'Insiemi di opzioni che l''utente cambia raramente e cerca per nome: preferenze, profilo, integrazioni, notifiche. Se le opzioni sono meno di 6, non serve questo pattern: bastano in una schermata sola.',
 'Navigazione per sezioni + un''area di contenuto.
NAVIGAZIONE (colonna a sinistra o schede in alto): le sezioni con nomi che dicono cosa contengono ("Notifiche", non "Generali 2").
AREA DI CONTENUTO: una sezione per volta. Ogni impostazione su una riga: a sinistra etichetta e, sotto, una riga che spiega COSA cambia; a destra il controllo. Le impostazioni pericolose o irreversibili stanno in fondo, separate e marcate.
SALVATAGGIO: scegli UNA strada e rendila evidente. O salvataggio immediato al cambio (con conferma visibile per ogni riga), o pulsante "Salva" con l''avviso delle modifiche non salvate. Le due strade mescolate sono il difetto tipico di questo pattern: l''utente non sa se ha salvato.',
 '- CARICAMENTO dei valori attuali: i controlli sono disabilitati finche'' non si sa il valore vero, per non mostrare un default che l''utente scambia per la sua impostazione;
- SALVATAGGIO IN CORSO sulla singola riga (se il salvataggio e'' immediato) o sul pulsante;
- ESITO del salvataggio: conferma breve e visibile in caso di successo; in caso di errore, il controllo TORNA al valore precedente e il messaggio dice che non e'' stato salvato. Un controllo che resta sul valore nuovo dopo un errore mente all''utente;
- MODIFICHE NON SALVATE: se esiste il pulsante "Salva", uscendo dalla sezione va avvertito;
- AZIONE IRREVERSIBILE: conferma esplicita che nomina cio'' che verra'' perso.',
 'NON mescolare salvataggio immediato e pulsante "Salva" nella stessa schermata.
NON lasciare un controllo sul valore nuovo se il salvataggio e'' fallito.
NON mettere le azioni distruttive accanto a quelle ordinarie.
NON usare etichette che non dicono cosa cambia ("Modalita'' avanzata" senza spiegazione).
NON mostrare i controlli prima di conoscere i valori attuali.')

ON CONFLICT (key) DO UPDATE SET
    app_type = EXCLUDED.app_type,
    title = EXCLUDED.title,
    when_to_use = EXCLUDED.when_to_use,
    structure = EXCLUDED.structure,
    required_states = EXCLUDED.required_states,
    anti_patterns = EXCLUDED.anti_patterns,
    updated_at = NOW();

-- ─────────────────────────────────────────────────────────────────────────────
-- (2) Purpose model della figura (tier-based, regola G: provider/model_id sono
--     fallback degenere, mai una scelta). Lente di analisi come le altre non
--     critiche -> tier 'medium', capability 'reasoning', tool read-only.
-- ─────────────────────────────────────────────────────────────────────────────
INSERT INTO nexus_purpose_model (purpose, provider, model_id, tier, required_capability, requires_tool_use, notes) VALUES
    ('council_ui_ux_designer', 'deepseek', 'deepseek-v4-flash', 'medium', 'reasoning', true,
     'Consiglio: ui_ux_designer, medium/reasoning (mig 0650)')
ON CONFLICT (purpose) DO UPDATE SET
    tier = EXCLUDED.tier,
    required_capability = EXCLUDED.required_capability,
    requires_tool_use = EXCLUDED.requires_tool_use,
    notes = EXCLUDED.notes,
    updated_at = NOW();

-- ─────────────────────────────────────────────────────────────────────────────
-- (3) Definizione del kind. Sola lettura come le altre figure, piu'' il tool
--     `ui_layout_patterns`: e'' cio'' che distingue un parere che cita una
--     struttura da uno che esprime un gusto.
-- ─────────────────────────────────────────────────────────────────────────────
INSERT INTO nexus_subagent_definitions (kind, description, prompt_key, tool_whitelist, model_purpose, max_iterations, timeout_s, is_background) VALUES
    ('ui_ux_designer',
     'Figura di analisi (read-only): usabilita'', gerarchia visiva, layout, stati di vuoto/caricamento/errore, accessibilita'' minima, coerenza fra le schermate.',
     'subagent.ui_ux_designer.base',
     ARRAY['read_file','search_in_files','list_files','search_codebase_semantic','recall_context','nexus_search_semantic','knowledge_search','ui_layout_patterns'],
     'council_ui_ux_designer', 12, 240, false)
ON CONFLICT (kind) DO UPDATE SET
    description = EXCLUDED.description,
    prompt_key = EXCLUDED.prompt_key,
    tool_whitelist = EXCLUDED.tool_whitelist,
    model_purpose = EXCLUDED.model_purpose,
    max_iterations = EXCLUDED.max_iterations,
    timeout_s = EXCLUDED.timeout_s,
    updated_at = NOW();

-- ─────────────────────────────────────────────────────────────────────────────
-- (4) Prompt XML della figura (schema standard CLAUDE.md sez. D: e'' un prompt
--     FUORI chat, quindi autonomia/protocollo/output esplicitati).
--
--     La sezione <cosa_e_bloccante> e'' la parte che tiene: il review panel gia''
--     oggi boccia su rilievi di UX minori mentre non si accorgeva che l''app
--     rispondeva 500 su ogni scrittura. Una lente estetica senza criteri
--     verificabili aggiunge rimandi a vuoto, che costano (un run del 27/07: 3
--     rimandi, 2,1M token, 3,08 USD). Qui il confine e'' scritto.
-- ─────────────────────────────────────────────────────────────────────────────
INSERT INTO nexus_prompt_templates (key, category, title, content, is_active, version, updated_by, updated_at) VALUES
('subagent.ui_ux_designer.base', 'automation', 'Consiglio: ui_ux_designer',
$$<role>Sei il progettista di interfacce nel consiglio di analisi Nexus. Guardi la richiesta dal punto di vista di chi USERA'' il risultato: cosa vede, cosa capisce, cosa puo'' fare e cosa succede quando qualcosa va storto. NON scrivi ne'' esegui codice: definisci i vincoli di interfaccia che l''esecuzione dovra'' rispettare.</role>

<contesto>
Ricevi una richiesta isolata + il blocco di contesto (memoria_progetto, rationale_parent). Altre figure la analizzano in parallelo con lenti diverse (requisiti, architettura, sicurezza, infrastruttura); un coordinatore fara' convergere i pareri. Sei l''unica voce che guarda l''interfaccia: se non lo fai tu, non lo fa nessuno.
</contesto>

<lente>
- Che tipo di schermata serve davvero: elenco con form, panoramica, procedura a passi, elenco-dettaglio, impostazioni. Dichiaralo, perche' da questo discende tutto il resto.
- Gerarchia visiva: qual e' l''azione primaria di ogni schermata (UNA), cosa e' secondario, cosa puo' stare in fondo.
- Stati: cosa vede l''utente mentre i dati caricano, quando non ce ne sono, quando la ricerca non trova nulla, quando la chiamata fallisce. Sono la parte piu' spesso dimenticata e la piu' facile da verificare.
- Percorsi: da dove si entra, come si torna indietro, cosa succede dopo un salvataggio o un errore. Un''azione senza esito visibile e' un difetto.
- Accessibilita' minima: ogni campo ha un''etichetta vera; il testo si legge sul suo sfondo; cio' che si puo' fare col mouse si puo' fare da tastiera; nessuna informazione affidata al solo colore.
- Coerenza: le schermate della stessa app si somigliano (stesse posizioni, stessi nomi per la stessa azione).
</lente>

<autonomia>
- Tool di sola lettura: read_file, search_in_files, list_files, search_codebase_semantic, recall_context, nexus_search_semantic, knowledge_search, ui_layout_patterns.
- PRIMA di dare il parere chiama ui_layout_patterns (senza argomenti per l''indice, poi con app_type per la scheda completa del tipo pertinente). Il tuo parere deve CITARE il pattern applicabile per chiave: e' cio' che distingue un vincolo applicabile da un aggettivo.
- Se il progetto ha gia' delle schermate, leggile prima di proporre: la coerenza con cio' che esiste vale piu' di un pattern astratto.
- Se la richiesta o il contesto indicano uno stile, un riferimento o un sistema di componenti gia' adottato, quello VINCE sul catalogo: il tuo compito e' applicarlo con coerenza, non sostituirlo col tuo gusto.
</autonomia>

<cosa_e_bloccante>
Distingui due cose, e non confonderle mai: cio' che RENDE L''INTERFACCIA INUSABILE (bloccante) e cio' che la renderebbe piu' bella (suggerimento).

E' BLOCCANTE (severity alta) solo cio' che si puo' VERIFICARE guardando il codice, e solo se ricade in questi casi:
- uno stato obbligatorio del pattern applicabile non e' reso: nessun caricamento, nessuno stato vuoto, nessuna gestione visibile dell''errore;
- un''azione non ha esito visibile: l''utente non puo' sapere se e' andata a buon fine o no;
- un elemento necessario al compito non e' raggiungibile (fuori schermo, coperto, senza modo di arrivarci);
- un campo non ha etichetta, o l''errore di validazione non e' associato al campo;
- un''azione distruttiva senza conferma;
- il compito richiesto non e' completabile dall''interfaccia proposta.

NON e' bloccante (al massimo suggerimento, severity media o bassa): scelte di colore, spaziature, tipografia, animazioni, "sarebbe piu' moderno", "meglio delle card di una tabella", preferenze fra due soluzioni entrambe funzionanti. Su queste puoi dare una raccomandazione, mai un veto.

Ogni rilievo bloccante deve portare l''EVIDENZA (quale file, quale schermata, quale stato manca) e il pattern del catalogo che lo richiede. Un rilievo senza evidenza verificabile e' un suggerimento, e va dichiarato come tale.
</cosa_e_bloccante>

<principi_nexus>
- Regola L (punto unico): se il progetto ha gia' un componente per questa cosa, si riusa; segnala la duplicazione di componenti come rischio.
- Non chiedere lavoro che la richiesta non implica: se l''utente ha chiesto una schermata, non pretendere un sistema di design.
</principi_nexus>

<anti_loop>
Un solo giro di analisi mirata: catalogo, schermate esistenti se ci sono, parere. Non ri-esplorare in cerca di certezza assoluta.
</anti_loop>

<output_format>
Concludi chiamando il tool advisory_verdict:
- requirements: i vincoli di interfaccia che l''esecuzione DEVE rispettare, ciascuno concreto e verificabile (quale schermata, quale stato, quale gerarchia). Cita la chiave del pattern applicabile.
- risks: i rilievi con severity alta|media|bassa ed evidenza. 'alta' SOLO per i casi elencati in <cosa_e_bloccante>.
- recommendations: i miglioramenti non bloccanti.
- verdict: proceed | proceed_with_changes | block. Usa 'block' solo con un rischio 'alta' documentato; il default di una lente di interfaccia e' 'proceed_with_changes'. Un block senza alcun rischio con evidenza viene RIFIUTATO dal sistema.
Il final_answer in prosa resta il resoconto umano; il parere macchina e' SOLO quello del tool.
Niente dump di codice, niente CSS: descrivi struttura e comportamento, non l''implementazione.
</output_format>$$,
true, 1, 'system', NOW())
ON CONFLICT (key) DO UPDATE SET
    content = EXCLUDED.content,
    updated_at = NOW(),
    updated_by = 'migration_0650';

-- ─────────────────────────────────────────────────────────────────────────────
-- (5) Whitelist runtime dei kind (Guard 1 del dispatcher): senza, il dispatch
--     rifiuterebbe il kind nonostante la definition esista.
--     Idempotente: split del CSV + append + DISTINCT.
-- ─────────────────────────────────────────────────────────────────────────────
UPDATE settings
   SET value = (
       SELECT string_agg(k, ',' ORDER BY k)
       FROM (
           SELECT DISTINCT trim(x) AS k
           FROM unnest(
               string_to_array(COALESCE(value, ''), ',') || ARRAY['ui_ux_designer']
           ) AS x
           WHERE trim(x) <> ''
       ) t
   ),
       updated_at = NOW()
 WHERE key = 'orchestrator.subagent_kinds_whitelist';

-- ─────────────────────────────────────────────────────────────────────────────
-- (6) Attivazione della figura: l''asse d''ambito "ui".
--
--     Gli assi d''ambito diventano un DATO (`council_domain_axes`): prima
--     l''unico asse, infra, era scritto a mano nel selettore, e un secondo
--     ambito avrebbe richiesto di ricopiarne il ramo (regola L). Ogni asse
--     legge le proprie chiavi `orchestrator.council_<nome>_{figures,keywords}`.
--
--     Le keyword sono confrontate a PAROLA INTERA (non piu' a sottostringa):
--     e' quello che rende utilizzabile un vocabolario fatto di parole corte —
--     'app', 'ui', 'form', 'web'. A sottostringa 'app' avrebbe trovato
--     'approccio' e 'form' avrebbe trovato 'informazioni'.
-- ─────────────────────────────────────────────────────────────────────────────
INSERT INTO settings (key, value, category, description) VALUES
  ('orchestrator.council_domain_axes', 'infra,ui', 'orchestrator',
   'Consiglio delle Competenze: assi d''ambito attivi (CSV). Ogni asse <nome> legge orchestrator.council_<nome>_figures e orchestrator.council_<nome>_keywords. Un asse nuovo e'' una riga qui piu'' le sue due chiavi: nessun codice.'),
  ('orchestrator.council_ui_figures', 'ui_ux_designer', 'orchestrator',
   'Consiglio delle Competenze: figure convocate quando il task riguarda un''interfaccia.'),
  ('orchestrator.council_ui_keywords',
   'app,applicazione,applicazioni,interfaccia,interfacce,frontend,front-end,ui,ux,usabilita,usabilità,accessibilita,accessibilità,layout,pagina,pagine,schermata,schermate,videata,dashboard,cruscotto,form,wizard,modale,popup,sidebar,navbar,header,footer,menu,navigazione,pulsante,bottone,css,tailwind,shadcn,bootstrap,material,figma,mockup,wireframe,design,grafica,tema,responsive,mobile,desktop,react,vue,svelte,angular,next.js,nextjs,sito,web,portale,gestionale,landing,crud',
   'orchestrator',
   'Consiglio delle Competenze: vocabolario d''ambito interfaccia. Match a PAROLA INTERA, case-insensitive (punto unico prompt_templates::touches_domain_keyword).')
ON CONFLICT (key) DO NOTHING;

-- ─────────────────────────────────────────────────────────────────────────────
-- (7) Il gate che CHIEDE quando l''indicazione manca.
--
--     Se la richiesta costruisce un''interfaccia e non dice come deve essere,
--     chiedere costa un turno; indovinare costa un run intero e il risultato va
--     rifatto. Il gate riusa il canale della disambiguazione (il turno si ferma
--     senza far partire un secondo giro LLM) e il VOCABOLARIO D''AMBITO della
--     figura UI: un solo elenco per la domanda "questo task riguarda
--     l''interfaccia?" (regola L).
--
--     Il secondo vocabolario e'' l''opposto: i segnali che un''indicazione C''E''
--     GIA''. Non contiene 'layout' ne'' 'design' — sono in council_ui_keywords e
--     dicono di COSA si parla, non COME deve essere fatto: metterli qui
--     spegnerebbe il gate proprio sulle richieste che lo motivano.
--     Non contiene 'come': troppo frequente in italiano fuori contesto
--     ("non so come farlo") per valere come indicazione di stile.
-- ─────────────────────────────────────────────────────────────────────────────
INSERT INTO settings (key, value, category, description) VALUES
  ('agent.ui_style_clarification_enabled', 'true', 'agent',
   'Chiede all''utente un riferimento di stile/layout quando la richiesta costruisce un''interfaccia e non ne indica nessuno. Salta in modalita'' automatic, con allegati presenti, e se la domanda e'' gia'' stata posta nella sessione. Codice: mcp-core::ui_clarification.'),
  ('agent.ui_style_indication_keywords',
   'stile,stili,mockup,wireframe,figma,screenshot,schizzo,riferimento,riferimenti,simile,somiglia,somigliante,ispirato,ispirata,ispirazione,palette,colori,colore,font,tipografia,material ui,tailwind,shadcn,bootstrap,chakra,ant design,mantine,daisyui,design system,sistema di design,tema scuro,tema chiaro,dark mode,light mode,minimal,minimalista,essenziale,moderno,elegante,sobrio,professionale,brand,logo,mobile-first,solo desktop,responsive',
   'agent',
   'Segnali che la richiesta indica GIA'' uno stile o un riferimento visivo: se il testo ne tocca uno, il gate non chiede. Match a PAROLA INTERA (prompt_templates::touches_domain_keyword).')
ON CONFLICT (key) DO NOTHING;

-- ─────────────────────────────────────────────────────────────────────────────
-- (8) L''ultimo anello: chi IMPLEMENTA deve sapere che il catalogo esiste.
--
--     Senza questa direttiva la catena si spezza proprio in fondo: la figura
--     cita il pattern per chiave ("crud_lista_form_dettaglio"), il parere arriva
--     nel prompt del run — e li' quella chiave e'' una parola senza scheda. Il
--     tool `ui_layout_patterns` non e'' riservato ai sub-agent proprio per
--     questo (vedi SUBAGENT_ONLY_TOOLS).
--
--     Sezione breve e condizionata al caso: entra nel prompt di ogni run, e un
--     prompt e'' un costo per turno. Append solo se non gia'' presente.
-- ─────────────────────────────────────────────────────────────────────────────
UPDATE nexus_prompt_templates
   SET content = content || $md$

<interfacce>
Quando costruisci o modifichi un'interfaccia (pagine, viste, componenti, schermate):
- se un parere del consiglio cita un pattern di layout per chiave, chiama ui_layout_patterns con l'app_type corrispondente e SEGUI quella scheda: struttura, gerarchia e stati obbligatori sono li';
- se nessuno l'ha citato, chiamalo comunque prima di inventare una struttura: costa una chiamata e ti dice quali stati vanno resi;
- gli stati elencati come obbligatori dal pattern (caricamento, elenco vuoto, errore) vanno implementati, non rimandati: sono la differenza fra una schermata usabile e una che sembra rotta al primo avvio;
- se la richiesta indica uno stile, un riferimento o una libreria di componenti, quello vince sul catalogo.
</interfacce>$md$,
       updated_at = NOW(),
       updated_by = 'migration_0650'
 WHERE key IN ('system.nexus_base', 'agent.coder.base')
   AND content NOT LIKE '%<interfacce>%';

-- Il catalogo e'' di sola lettura, quindi vale anche in modalita'' Studio: li'
-- si analizza senza scrivere, ed e'' proprio dove serve poter dire "questa
-- schermata dovrebbe seguire questo pattern" prima di toccare il codice.
UPDATE settings
   SET value = value || ',ui_layout_patterns',
       updated_at = NOW()
 WHERE key = 'automation.study_mode_readonly_tools'
   AND value NOT LIKE '%ui_layout_patterns%';

-- Il cap sale a 7: cinque figure base piu'' i due assi d''ambito, quando un task
-- li tocca entrambi e nessun piano di orchestrazione ha gia'' dimensionato il
-- panel. Con un target del piano il totale resta quello del target (le figure
-- d''ambito prendono il posto delle base, non si sommano).
UPDATE settings
   SET value = '7',
       description = 'Consiglio delle Competenze: cap massimo di figure convocate in un pre-step. '
                     'Backstop assoluto: quando il piano di orchestrazione dichiara un target, '
                     'e'' quello a dimensionare il panel.',
       updated_at = NOW()
 WHERE key = 'orchestrator.council_max_figures'
   AND value = '6';
