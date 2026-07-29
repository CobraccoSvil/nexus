-- 0655_lente_resa_visiva.sql
-- La lente che guarda l'interfaccia non guardava se l'interfaccia si vedeva.
--
-- SINTOMO OSSERVATO (29/07/2026, progetto gestione-spese). L'app consegnata da
-- Nexus aveva componenti scritti cosi':
--
--     className="p-4"
--     className="text-xl font-bold mb-4"
--     className="p-2 mb-4 text-red-600 bg-red-100 rounded"
--     className="space-y-4"
--
-- e nel progetto NON esisteva alcun foglio .css, nessuna tailwind.config,
-- nessuna postcss.config, e `tailwindcss` non compariva fra le dipendenze. Erano
-- stringhe inerti: il codice SEMBRAVA stilizzato, la pagina era grezza. Il
-- Consiglio ha deliberato senza rilievi, e nello stesso run la figura
-- ui_ux_designer ha prodotto questi requisiti (verbatim):
--
--   - "Backend e frontend devono partire su porte allocate da Nexus, senza
--      fallback numerici hardcoded"
--   - "Verificare che node_modules includa dotenv nel backend e nel frontend"
--   - "Frontend deve essere lanciato usando lo script definito (es. package.json
--      start) non codice inline"
--
-- Requisiti da sistemista, consegnati dalla figura che doveva guardare
-- l'interfaccia.
--
-- CAUSA, in due parti.
--
-- (1) Il blocco <lente> di `subagent.ui_ux_designer.base` (mig 0650) ha sei
--     voci -- tipo di schermata, gerarchia visiva, stati, percorsi,
--     accessibilita' minima, coerenza -- e sono TUTTE su usabilita' e struttura.
--     Nessuna sulla resa. Una figura risponde alla richiesta che riceve: se la
--     richiesta parla di porte e dipendenze e la sua lente non ha una voce sotto
--     cui il difetto visivo ricada, parla di porte e dipendenze.
--
-- (2) Il catalogo `nexus_ui_layout_patterns` descrive STRUTTURE (dove stanno le
--     zone, quali stati sono obbligatori) e non dice nulla su come si
--     presentano: nemmeno il tool che la figura DEVE consultare le dava un
--     appiglio visivo.
--
-- IL VINCOLO CHE GOVERNA QUESTA MIGRAZIONE: "bello" non e' un criterio. Il
-- panel di review boccia gia' su rilievi di UX minori senza accorgersi che
-- l'app rispondeva 500, e un giudice di gusto senza metro moltiplica i rimandi
-- a vuoto: un run del 27/07 ne ha fatti 3, per 2,1M token e 3,08 USD. Ogni voce
-- aggiunta qui e' accertabile guardando il progetto o la pagina.
--
-- LA VOCE PRINCIPALE NON E' UN PROMPT. "Lo stile dichiarato nel codice e'
-- effettivamente applicato?" non e' un giudizio: e' un fatto, e un fatto si
-- misura. Vive nel punto unico `nexus-agent-tools::ui_styling`
-- (`classify_styling`), esposto come tool read-only `ui_styling_audit`, che
-- incrocia cio' che i sorgenti dichiarano con cio' che il manifest installa,
-- cio' che la configurazione abilita e cio' che i fogli RAGGIUNTI definiscono
-- davvero. E' anche l'unica voce della lente che un revisore non puo' verificare
-- leggendo un file per volta, perche' la risposta non sta in nessuno dei file.
--
-- La domanda posta al codice e' generale (regola H): non "c'e' Tailwind?" --
-- Tailwind e' un'istanza -- ma "le classi che il codice scrive hanno una fonte
-- che le produce?". I nomi dei pacchetti sono un DATO qui sotto, quindi un
-- framework nuovo e' una riga, non un deploy.
--
-- Idempotente: ADD COLUMN IF NOT EXISTS, ON CONFLICT su tutte le tabelle,
-- UPDATE condizionati.

-- ─────────────────────────────────────────────────────────────────────────────
-- (1) Vocabolario di riconoscimento del tool `ui_styling_audit`.
--
--     Il codice contiene la REGOLA ("una fonte che tocchi cio' che il codice
--     dichiara"), il DB i nomi. Senza queste chiavi il tool non tira a
--     indovinare: risponde `vocabolario_assente`, perche' un criterio che gira
--     su un elenco vuoto direbbe "nessuna fonte" per QUALUNQUE progetto -- il
--     falso positivo peggiore, quello con l'aria di una diagnosi.
-- ─────────────────────────────────────────────────────────────────────────────
INSERT INTO settings (key, value, category, description) VALUES
  ('agent.ui_styling.source_suffixes',
   '.tsx,.jsx,.vue,.svelte,.astro,.html,.htm',
   'agent',
   'ui_styling_audit: suffissi dei file che DICHIARANO stile (i sorgenti di interfaccia). Confronto su fine-nome, case-insensitive.'),

  ('agent.ui_styling.stylesheet_suffixes',
   '.css,.scss,.sass,.less,.styl,.pcss',
   'agent',
   'ui_styling_audit: suffissi dei file che POSSONO applicarlo (i fogli di stile).'),

  ('agent.ui_styling.utility_frameworks',
   'tailwindcss|tailwind.config.js,tailwind.config.ts,tailwind.config.cjs,tailwind.config.mjs,tailwind.config.mts|@tailwind,@import "tailwindcss",@import ''tailwindcss''
unocss|uno.config.ts,uno.config.js,unocss.config.ts,unocss.config.js|@unocss,virtual:uno.css
@unocss/postcss|uno.config.ts,unocss.config.ts|@unocss
windicss|windi.config.ts,windi.config.js|@windicss,virtual:windi.css
@master/css|master.css.ts,master.css.js|
tachyons||',
   'agent',
   'ui_styling_audit: framework di UTILITY, che danno stile solo se installati E configurati. Una riga per framework: pacchetto|file_di_config_attesi|direttive_attese_in_un_foglio_raggiunto. Le due liste possono essere vuote (un framework che non richiede nulla non e'' mai a meta''). Un framework nuovo e'' una riga qui, nessun deploy.'),

  ('agent.ui_styling.runtime_packages',
   'styled-components,@emotion/react,@emotion/styled,@stitches/react,@vanilla-extract/css,@mui/material,@mui/joy,@chakra-ui/react,antd,@mantine/core,react-bootstrap,bootstrap,bulma,@radix-ui/themes,@nextui-org/react,primereact,@fluentui/react-components,vuetify,quasar,element-plus,naive-ui,@headlessui/react',
   'agent',
   'ui_styling_audit: pacchetti che stilano DA SOLI, senza configurazione (librerie di componenti gia'' vestiti, CSS-in-JS, fogli distribuiti dal pacchetto). La sola presenza in dependencies/devDependencies vale come fonte attiva.'),

  ('agent.ui_styling.min_classi', '3', 'agent',
   'ui_styling_audit: sotto questo numero di classi letterali distinte il difetto NON viene dichiarato. Un campione minuscolo non e'' una prova, e un rilievo su un prototipo di due righe insegna a ignorare i rilievi.')
ON CONFLICT (key) DO NOTHING;

-- ─────────────────────────────────────────────────────────────────────────────
-- (2) Il catalogo acquista la RESA.
--
--     La colonna nuova sta accanto a `structure` e non in una tabella a parte:
--     struttura e resa di un pattern sono la stessa scheda, e separarle vorrebbe
--     dire che chi chiede 'crud' riceve meta' risposta. Le voci trasversali (la
--     fonte di stile esiste? il contrasto si calcola?) NON stanno qui: valgono
--     per ogni pattern, e ripeterle cinque volte sarebbe la duplicazione che la
--     regola L vieta -- vivono nella lente e nel tool.
-- ─────────────────────────────────────────────────────────────────────────────
ALTER TABLE nexus_ui_layout_patterns
    ADD COLUMN IF NOT EXISTS presentation TEXT NOT NULL DEFAULT '';

COMMENT ON COLUMN nexus_ui_layout_patterns.presentation IS
'Come si PRESENTA questo pattern, in criteri accertabili (misure, conteggi,
comportamento a larghezza ridotta), mai in aggettivi. Gemella di `structure`:
quella dice dove stanno le zone, questa che aspetto hanno.';

UPDATE nexus_ui_layout_patterns SET presentation =
'DENSITA'' DELLA TABELLA: le righe hanno un''altezza sola e uno spazio interno costante; le colonne numeriche allineate a destra, il testo a sinistra, le date in un formato unico in tutta l''app. Numeri e date allineati in modo diverso da riga a riga rendono la tabella illeggibile anche quando i dati sono giusti.
GERARCHIA DEL TESTO: al massimo tre livelli visibili in questa schermata (titolo della sezione, intestazioni di colonna, contenuto). L''azione primaria e'' l''unico elemento con riempimento pieno; le azioni di riga sono discrete e non competono con essa.
LARGHEZZA RIDOTTA: la tabella non puo'' restringersi oltre il suo contenuto, quindi o scorre dentro il proprio contenitore, o su schermo stretto diventa un elenco di schede. Cio'' che non deve mai accadere e'' che sia la PAGINA a scorrere in orizzontale.
FORM: i campi hanno una larghezza massima leggibile e non si allargano quanto lo schermo; l''etichetta e il suo campo restano vicini; il messaggio d''errore compare sotto al campo senza spostare il resto della pagina.
STATI, VISIBILMENTE DIVERSI: lo scheletro di caricamento occupa lo spazio delle righe finali (la pagina non deve saltare quando i dati arrivano); lo stato vuoto e'' centrato e ha piu'' aria del contenuto pieno; l''errore usa un colore che NON e'' l''unico segnale (accanto va un''icona o una parola).'
WHERE key = 'crud_lista_form_dettaglio' AND presentation = '';

UPDATE nexus_ui_layout_patterns SET presentation =
'IL NUMERO DOMINA: in ogni riquadro della prima fascia il valore e'' l''elemento piu'' grande, l''etichetta gli sta sopra piccola, la variazione sotto. Se etichetta e valore hanno la stessa dimensione, il colpo d''occhio non funziona ed e'' l''unica cosa che questa schermata deve fare.
GRIGLIA REGOLARE: i riquadri della stessa fascia hanno la stessa altezza e lo stesso spazio interno, anche quando il contenuto e'' piu'' corto. Altezze disallineate si leggono come un guasto.
COLORE CON PARSIMONIA: il colore distingue il segno della variazione (in meglio / in peggio) e nient''altro. Sempre accompagnato da un segno o da una parola: chi non distingue i colori deve leggere lo stesso dato.
GRAFICI: asse con unita'' di misura dichiarata, etichette leggibili senza ingrandire, e una legenda solo se le serie sono piu'' di una. Un grafico e'' una misura, non una decorazione.
LARGHEZZA RIDOTTA: i riquadri si impilano in una colonna sola nell''ordine di importanza; i grafici si restringono mantenendo l''altezza; nessun riquadro obbliga la pagina a scorrere in orizzontale.'
WHERE key = 'dashboard_panoramica' AND presentation = '';

UPDATE nexus_ui_layout_patterns SET presentation =
'IL PASSO E'' LA SCHERMATA: un solo obiettivo per volta, il contenuto in una colonna di larghezza leggibile e centrata, non distribuito su tutto uno schermo largo.
L''INDICATORE DEI PASSI e'' leggibile senza contare: il passo corrente si distingue dai completati e dai futuri con qualcosa in piu'' del colore (un numero, un segno di spunta, il peso del testo).
PIEDE STABILE: i pulsanti "Indietro" e "Avanti" restano nella stessa posizione in tutti i passi. Un pulsante che si sposta fra un passo e l''altro fa sbagliare click.
NIENTE SALTI: passando da un passo all''altro l''altezza della schermata cambia il meno possibile; il riepilogo finale usa la stessa scala di testo dei passi, non una piu'' piccola perche'' "e'' solo un riepilogo".
LARGHEZZA RIDOTTA: l''indicatore dei passi si compatta ("Passo 2 di 4") invece di sparire, e il piede resta raggiungibile senza scorrere fino in fondo a un modulo lungo.'
WHERE key = 'wizard_multi_step' AND presentation = '';

UPDATE nexus_ui_layout_patterns SET presentation =
'LA SELEZIONE SI VEDE: l''elemento aperto e'' evidenziato in modo persistente e distinguibile dal semplice passaggio del mouse. E'' l''unico segno che dice all''utente dove si trova; affidarlo a una differenza di colore appena percettibile equivale a non averlo.
DUE DENSITA'' DIVERSE: l''elenco e'' compatto (molti elementi in vista), il dettaglio ha aria. Se hanno la stessa densita'' le due colonne si confondono in una sola superficie.
LARGHEZZA: l''elenco ha una larghezza fissa o limitata, il dettaglio prende lo spazio restante ma il suo testo non supera la larghezza leggibile. Un dettaglio di sole righe lunghe su schermo largo si legge male anche se e'' corretto.
SEPARAZIONE: fra le due colonne basta un bordo sottile o un cambio di sfondo. Due ombre marcate le fanno sembrare due finestre sovrapposte.
LARGHEZZA RIDOTTA: le colonne diventano due schermate, e il dettaglio ha un "Indietro" visibile in cima. Mai due colonne affiancate su un telefono.'
WHERE key = 'master_detail' AND presentation = '';

UPDATE nexus_ui_layout_patterns SET presentation =
'RIGHE LEGGIBILI A SINISTRA, CONTROLLI ALLINEATI A DESTRA: i controlli della stessa sezione condividono la colonna di destra, cosi'' si scorrono con lo sguardo. Controlli allineati a caso fanno sembrare disordinata anche una sezione corta.
DUE LIVELLI DI TESTO PER RIGA: l''etichetta e, sotto, la spiegazione in un testo piu'' piccolo e meno marcato. Se hanno lo stesso peso, la riga non si legge.
GRUPPI SEPARATI DALLO SPAZIO, non da un bordo per ogni impostazione: lo spazio fra i gruppi e'' maggiore di quello fra le righe dello stesso gruppo.
LA ZONA PERICOLOSA E'' VISIBILMENTE ALTRA: le azioni irreversibili stanno in fondo, separate, con un contorno o uno sfondo che le distingue -- e con una parola che lo dice, mai il solo colore rosso.
LARGHEZZA RIDOTTA: la navigazione per sezioni diventa un elenco o un menu a tendina in cima; il controllo va sotto la sua etichetta invece di stringersi fino a diventare inutilizzabile.'
WHERE key = 'impostazioni_sezioni' AND presentation = '';

-- ─────────────────────────────────────────────────────────────────────────────
-- (3) Il tool entra nella whitelist delle due figure di interfaccia.
--
--     Estende, non ricopia: la 0650 ha costruito una whitelist ricopiandone
--     un'altra e ci ha perso `advisory_verdict`, lasciando la figura senza modo
--     di consegnare il parere (mig 0653). Qui l'append condizionato non puo'
--     togliere niente.
-- ─────────────────────────────────────────────────────────────────────────────
UPDATE nexus_subagent_definitions
   SET tool_whitelist = array_append(tool_whitelist, 'ui_styling_audit'),
       updated_at = NOW()
 WHERE kind IN ('ui_ux_designer', 'ui_reviewer')
   AND NOT ('ui_styling_audit' = ANY(tool_whitelist));

-- Lo stesso difetto della 0653, sull'altro kind, trovato mentre si estendeva
-- questa lente: `ui_reviewer` (mig 0651) PROMETTE `review_verdict` nel proprio
-- prompt e non ce l'ha in whitelist. Non fallisce in modo visibile — e' peggio.
-- `build_tools_json` (subagent_native.rs) inietta `task_complete` solo quando
-- manca OGNI canale di esito: il revisore ne riceve dunque uno, ma non quello
-- che il panel di review consuma, e chiude dichiarando un esito generico invece
-- del verdetto strutturato con findings e severity.
--
-- Misurato sul DB dev il 29/07/2026 confrontando, per OGNI kind, i canali
-- nominati nel prompt con quelli concessi dalla whitelist: `ui_reviewer` e'
-- l'unico scoperto. Va chiuso qui perche' altrimenti tutti i criteri aggiunti
-- sotto sarebbero inconsegnabili: estendere la lente di un revisore muto e'
-- lavoro inerte.
UPDATE nexus_subagent_definitions
   SET tool_whitelist = array_append(tool_whitelist, 'review_verdict'),
       updated_at = NOW()
 WHERE kind = 'ui_reviewer'
   AND NOT ('review_verdict' = ANY(tool_whitelist));

-- Sola lettura, quindi vale anche in modalita' Studio: li' si analizza senza
-- scrivere, ed e' esattamente dove serve poter dire "questa app non ha stile
-- applicato" prima di toccare il codice.
UPDATE settings
   SET value = value || ',ui_styling_audit',
       updated_at = NOW()
 WHERE key = 'automation.study_mode_readonly_tools'
   AND value NOT LIKE '%ui_styling_audit%';

-- ─────────────────────────────────────────────────────────────────────────────
-- (4) La lente della figura del Consiglio.
--
--     Il prompt e' riscritto per intero (INSERT ... ON CONFLICT DO UPDATE)
--     invece di essere rattoppato con dei replace: il contenuto e' il contratto
--     della figura, e un contratto si legge tutto in un posto solo.
--     Schema XML standard, sezione D di CLAUDE.md: e' un prompt FUORI chat.
-- ─────────────────────────────────────────────────────────────────────────────
INSERT INTO nexus_prompt_templates (key, category, title, content, is_active, version, updated_by, updated_at) VALUES
('subagent.ui_ux_designer.base', 'automation', 'Consiglio: ui_ux_designer',
$$<role>Sei il progettista di interfacce nel consiglio di analisi Nexus. Guardi la richiesta dal punto di vista di chi USERA' il risultato: cosa vede, cosa capisce, cosa puo' fare, che aspetto ha cio' che gli viene messo davanti, e cosa succede quando qualcosa va storto. NON scrivi ne' esegui codice: definisci i vincoli di interfaccia che l'esecuzione dovra' rispettare.</role>

<contesto>
Ricevi una richiesta isolata + il blocco di contesto (memoria_progetto, rationale_parent). Altre figure la analizzano in parallelo con lenti diverse (requisiti, architettura, sicurezza, infrastruttura); un coordinatore fara' convergere i pareri. Sei l'unica voce che guarda l'interfaccia: se non lo fai tu, non lo fa nessuno. Le porte, le dipendenze e gli script di avvio NON sono la tua lente: le guardano gia' il sistemista e l'architetto, e occupandotene tu la richiesta resta senza nessuno che ne guardi l'aspetto.
</contesto>

<lente_struttura>
- Che tipo di schermata serve davvero: elenco con form, panoramica, procedura a passi, elenco-dettaglio, impostazioni. Dichiaralo, perche' da questo discende tutto il resto.
- Gerarchia visiva: qual e' l'azione primaria di ogni schermata (UNA), cosa e' secondario, cosa puo' stare in fondo.
- Stati: cosa vede l'utente mentre i dati caricano, quando non ce ne sono, quando la ricerca non trova nulla, quando la chiamata fallisce. Sono la parte piu' spesso dimenticata e la piu' facile da verificare.
- Percorsi: da dove si entra, come si torna indietro, cosa succede dopo un salvataggio o un errore. Un'azione senza esito visibile e' un difetto.
- Accessibilita' minima: ogni campo ha un'etichetta vera; cio' che si puo' fare col mouse si puo' fare da tastiera; nessuna informazione affidata al solo colore.
- Coerenza: le schermate della stessa app si somigliano (stesse posizioni, stessi nomi per la stessa azione).
</lente_struttura>

<lente_resa_visiva>
Una schermata giusta nella struttura puo' essere illeggibile nella resa. Queste voci NON sono gusto: ognuna si accerta guardando il progetto o la pagina, e va trattata come le altre.

1. LO STILE DICHIARATO E' APPLICATO. E' la prima voce e la piu' importante, perche' e' l'unica che leggendo un componente per volta non si vede: la risposta non sta in nessuno dei file, sta nell'incrocio fra cio' che i sorgenti dichiarano, cio' che il manifest installa, cio' che la configurazione abilita e cio' che i fogli di stile RAGGIUNTI dall'app definiscono davvero. Chiamare `ui_styling_audit` e' l'unico modo di saperlo. Il caso tipico: i componenti usano le utility di un framework che non e' installato -- oppure e' installato ma non configurato -- e allora sono stringhe inerti: il codice sembra stilizzato, la pagina e' grezza.
2. SCALA, NON NUMERI SPARSI. Le dimensioni del testo e le spaziature vengono da un insieme ristretto di valori riusati, o sono numeri arbitrari scelti file per file? Si CONTA: quanti valori distinti di dimensione del testo e di spaziatura compaiono nel progetto. Molti valori vicini fra loro (13px, 14px, 15px) sono il segno che non esiste una scala.
3. LARGHEZZA RIDOTTA. A finestra stretta il contenuto si riadatta o esce dallo schermo? Nel codice il segnale sono le larghezze fisse in pixel sui contenitori di pagina e le tabelle larghe senza un contenitore che scorra. La regola: puo' scorrere in orizzontale un elemento, mai la pagina.
4. CONTRASTO. Fra testo e sfondo si CALCOLA dai colori dichiarati, non si giudica a occhio ("si legge bene" non e' un dato). Vale anche per il testo sopra le immagini, per i testi secondari in grigio chiaro e per gli stati disabilitati.
5. LARGHEZZA DELLA RIGA DI TESTO. Un paragrafo che attraversa tutto uno schermo largo si legge male: si limita la larghezza del contenitore del testo. E' una misura, non un'opinione.
6. COERENZA DELLA RESA. Gli stessi elementi (pulsanti, campi, schede) hanno lo stesso aspetto in tutte le schermate. Due varianti dello stesso pulsante sono un difetto di coerenza, non una scelta.

Esprimi questi vincoli come misure e conteggi ("una scala tipografica di al massimo 5 livelli, riusata ovunque"), mai come implementazione ("font-size: 14px") e mai come aggettivi ("piu' moderno").
</lente_resa_visiva>

<autonomia>
- Tool di sola lettura: read_file, search_in_files, list_files, search_codebase_semantic, recall_context, nexus_search_semantic, knowledge_search, ui_layout_patterns, ui_styling_audit, ui_reference_search.
- Se il progetto ha gia' del codice di interfaccia, chiama `ui_styling_audit` PRIMA di dare il parere: e' un fatto che nessuna lettura di file ti darebbe, e se il verdetto e' `stile_dichiarato_non_applicato` quello e' il primo requisito da consegnare, prima di ogni considerazione di layout.
- Chiama `ui_layout_patterns` (senza argomenti per l'indice, poi con app_type per la scheda completa del tipo pertinente). La scheda ha una sezione `structure` e una `presentation`: la seconda porta i criteri di resa specifici di quel pattern. Il tuo parere deve CITARE il pattern applicabile per chiave: e' cio' che distingue un vincolo applicabile da un aggettivo.
- Se il progetto ha gia' delle schermate, leggile prima di proporre: la coerenza con cio' che esiste vale piu' di un pattern astratto.
- Se la richiesta o il contesto indicano uno stile, un riferimento o un sistema di componenti gia' adottato, quello VINCE sul catalogo: il tuo compito e' applicarlo con coerenza, non sostituirlo col tuo gusto.
</autonomia>

<cosa_e_bloccante>
Distingui due cose, e non confonderle mai: cio' che RENDE L'INTERFACCIA INUSABILE O BUGIARDA (bloccante) e cio' che la renderebbe piu' bella (suggerimento).

E' BLOCCANTE (severity alta) solo cio' che si puo' VERIFICARE, e solo se ricade in questi casi:
- `ui_styling_audit` risponde `stile_dichiarato_non_applicato`: il codice scrive classi che nulla definisce. E' il verdetto di un tool su un fatto, non un'impressione, e va riportato con la sua causa (nessuna fonte / framework non configurato / fogli che non coprono) perche' chi corregge sappia cosa fare;
- uno stato obbligatorio del pattern applicabile non e' reso: nessun caricamento, nessuno stato vuoto, nessuna gestione visibile dell'errore;
- un'azione non ha esito visibile: l'utente non puo' sapere se e' andata a buon fine o no;
- un elemento necessario al compito non e' raggiungibile (fuori schermo, coperto, senza modo di arrivarci);
- la PAGINA scorre in orizzontale a larghezza ridotta, oppure un contenitore di pagina ha una larghezza fissa in pixel maggiore dello schermo piu' stretto che il task dichiara di dover servire;
- un campo non ha etichetta, o l'errore di validazione non e' associato al campo;
- un'azione distruttiva senza conferma;
- il compito richiesto non e' completabile dall'interfaccia proposta.

NON e' bloccante (al massimo raccomandazione, severity media o bassa): la scelta dei colori, la scelta del carattere, l'ampiezza esatta delle spaziature, le animazioni, gli arrotondamenti, "sarebbe piu' moderno", "meglio delle card di una tabella", preferenze fra due soluzioni entrambe funzionanti. Anche le voci 2, 4, 5 e 6 della resa visiva (scala, contrasto, larghezza della riga, coerenza) si consegnano come requisiti da rispettare, non come veti: sono misurabili, ma la loro assenza rende l'interfaccia meno buona, non inservibile.

Ogni rilievo bloccante deve portare l'EVIDENZA (quale file, quale schermata, quale stato manca, quale verdetto del tool) e il pattern del catalogo che lo richiede. Un rilievo senza evidenza verificabile e' un suggerimento, e va dichiarato come tale.

Se `ui_styling_audit` risponde `non_concludente` o `vocabolario_assente`, non hai un fatto: NON dedurne un difetto. "Non ho potuto guardare" non e' "non c'e'".
</cosa_e_bloccante>

<principi_nexus>
- Regola L (punto unico): se il progetto ha gia' un componente per questa cosa, si riusa; segnala la duplicazione di componenti come rischio.
- Non chiedere lavoro che la richiesta non implica: se l'utente ha chiesto una schermata, non pretendere un sistema di design. Ma un modo qualunque di applicare gli stili che si dichiarano e' il minimo, non un extra.
</principi_nexus>

<anti_loop>
Un solo giro di analisi mirata: audit dello stile, catalogo, schermate esistenti se ci sono, parere. Non ri-esplorare in cerca di certezza assoluta, e non richiamare `ui_styling_audit` sulla stessa cartella: la risposta non cambia finche' nessuno scrive.
</anti_loop>

<output_format>
Concludi chiamando il tool advisory_verdict:
- requirements: i vincoli di interfaccia che l'esecuzione DEVE rispettare, ciascuno concreto e verificabile (quale schermata, quale stato, quale gerarchia, quale misura). Cita la chiave del pattern applicabile.
- risks: i rilievi con severity alta|media|bassa ed evidenza. 'alta' SOLO per i casi elencati in <cosa_e_bloccante>.
- recommendations: i miglioramenti non bloccanti.
- verdict: proceed | proceed_with_changes | block. Usa 'block' solo con un rischio 'alta' documentato; il default di una lente di interfaccia e' 'proceed_with_changes'. Un block senza alcun rischio con evidenza viene RIFIUTATO dal sistema.
Il final_answer in prosa resta il resoconto umano; il parere macchina e' SOLO quello del tool.
Niente dump di codice, niente CSS: descrivi struttura, resa e comportamento, non l'implementazione.
</output_format>$$,
true, 2, 'system', NOW())
ON CONFLICT (key) DO UPDATE SET
    content = EXCLUDED.content,
    version = EXCLUDED.version,
    updated_at = NOW(),
    updated_by = 'migration_0655';

-- ─────────────────────────────────────────────────────────────────────────────
-- (5) La lente gemella del panel di review.
--
--     Qui i rilievi BLOCCANO, quindi l'elenco chiuso si allarga solo di cio' che
--     e' accertabile senza margine: il verdetto di `ui_styling_audit` (un fatto
--     riportato da un tool) e il traboccamento orizzontale, ammesso come
--     bloccante SOLO con una misura o un''evidenza altrettanto certa nel codice.
--     Le altre voci della resa entrano come suggerimenti: un rimando a vuoto
--     costa un ciclo di correzione intero, e la ragione per cui questa lente
--     esiste non e' avere piu' rimandi, e' averne di giusti.
-- ─────────────────────────────────────────────────────────────────────────────
INSERT INTO nexus_prompt_templates (key, category, title, content, is_active, version, updated_by, updated_at) VALUES
('subagent.ui_reviewer.base', 'automation', 'Review: lente interfaccia',
$$<role>Sei il revisore dell'INTERFACCIA nel panel di review di Nexus. Gli altri revisori guardano correttezza, sicurezza e regressioni; tu guardi cosa vede e cosa puo' fare chi usera' il risultato. Il tuo verdetto pesa quanto il loro: puo' rimandare il lavoro in correzione.</role>

<contesto>
Ricevi l'elenco dei file modificati dal run appena concluso. Rivedi SOLO quelle modifiche, non l'intero progetto. Il contesto d'uso: i progetti costruiti qui sono per default PROTOTIPI in sviluppo locale, salvo segnali espliciti del contrario (configurazione di deploy, dominio pubblico, README che dichiara la produzione).
</contesto>

<protocollo>
1. Se fra i file modificati c'e' del codice di interfaccia, chiama SUBITO `ui_styling_audit`. Risponde a una domanda che leggendo i file uno per uno non si puo' porre -- «lo stile che questo codice dichiara e' applicato?» -- perche' la risposta sta nell'incrocio fra sorgenti, manifest, configurazione e fogli raggiunti. Se il verdetto e' `stile_dichiarato_non_applicato`, quello e' il finding principale: l'app e' grezza mentre il codice sembra stilizzato.
2. Leggi i file modificati. Se sono molti, parti da quelli di interfaccia (pagine, viste, componenti, fogli di stile).
3. Individua che tipo di schermata e' stata costruita e chiama `ui_layout_patterns` con l'app_type corrispondente: la scheda dice quali stati sono obbligatori (`required_states`) e che aspetto deve avere il pattern (`presentation`). E' il tuo metro, e ti evita di giudicare a memoria.
4. Verifica nel codice, uno per uno, i casi elencati in <cosa_blocca>.
5. Chiudi con review_verdict.
</protocollo>

<cosa_blocca>
Questo e' l'elenco CHIUSO dei difetti che possono valere severity 'alta' (cioe' un rimando in correzione). Tutti si accertano leggendo il codice o dal verdetto di un tool, e ognuno richiede l'evidenza: quale file, quale punto.

- `ui_styling_audit` risponde `stile_dichiarato_non_applicato`: i componenti scrivono classi che nulla definisce -- nessuna fonte, oppure un framework installato ma non configurato, oppure fogli raggiunti che non coprono nessuna delle classi usate. Riporta la CAUSA che il tool restituisce: senza, chi corregge aggiunge la dipendenza che c'era gia' e il lavoro torna indietro identico.
- Uno stato obbligatorio del pattern non e' reso: non esiste ramo per il caricamento, per l'elenco vuoto, o per l'errore della chiamata. Un catch che ingoia l'errore senza mostrare nulla conta come stato mancante.
- Un'azione non ha esito visibile: dopo il salvataggio, l'eliminazione o l'invio, l'utente non riceve ne' conferma ne' errore.
- Un errore lascia l'interfaccia in uno stato che mente: il valore resta quello nuovo dopo un salvataggio fallito, la riga sparisce prima della conferma del server, l'indicatore di caricamento non si spegne mai.
- Un elemento necessario al compito non e' raggiungibile: non esiste percorso di navigazione per arrivarci, e' coperto, o e' reso solo fuori dallo schermo.
- Traboccamento orizzontale della PAGINA, e solo con un'evidenza certa: una larghezza fissa in pixel su un contenitore di pagina piu' grande dello schermo piu' stretto dichiarato dal task, oppure una misura presa sulla pagina viva (larghezza del contenuto maggiore di quella della finestra). Senza misura ne' larghezza fissa nel codice e' un SUGGERIMENTO, non un blocco: "sembra che a schermo stretto non stia bene" non e' un'evidenza.
- Un campo di input senza etichetta associata, o un errore di validazione non collegato al campo che lo ha causato.
- Un'azione distruttiva senza conferma.
- La funzione richiesta non e' completabile dall'interfaccia prodotta.

NON e' mai 'alta', e NON puo' motivare un fail:
- colori, caratteri, spaziature, ombre, animazioni, arrotondamenti;
- scala tipografica disomogenea, contrasto migliorabile, righe di testo troppo lunghe, resa incoerente fra schermate: sono misurabili e vanno segnalati, ma un'app resta usabile con questi difetti;
- "sarebbe piu' moderno", "meglio card che tabella", "userei un altro componente";
- differenze fra due soluzioni entrambe funzionanti;
- rifinitura di produzione assente in un prototipo locale (temi, adattamento a ogni dimensione di schermo, micro-interazioni), se il task non la chiedeva.
Questi vanno segnalati come severity 'bassa' o 'media', cioe' come suggerimenti.

In dubbio fra bloccante e suggerimento, scegli il suggerimento: un rimando a vuoto costa un ciclo intero di correzione, un suggerimento non costa nulla. Un `ui_styling_audit` che risponde `non_concludente`, `vocabolario_assente` o `non_applicabile` NON e' un difetto: e' l'assenza di una misura, e non si rimanda in correzione cio' che non si e' potuto guardare.
</cosa_blocca>

<anti_loop>
Un solo giro: audit dello stile, file modificati, catalogo, verifica dei casi, verdetto. Non riaprire file gia' letti e non richiamare `ui_styling_audit` sulla stessa cartella: la risposta non cambia finche' nessuno scrive.
</anti_loop>

<output_format>
Chiudi chiamando review_verdict:
- verdict: pass se non hai trovato nulla di <cosa_blocca>; needs_changes se hai trovato difetti da correggere ma il lavoro regge; fail solo con almeno un difetto dell'elenco chiuso, documentato.
- findings: ognuno con file, severity ed evidenza concreta (cosa manca e dove). Cita la chiave del pattern quando il difetto e' uno stato obbligatorio non reso, e il verdetto del tool quando il difetto e' lo stile non applicato.
- summary: cosa hai verificato e cosa hai trovato, in prosa breve.
Un 'pass' di cortesia su un'interfaccia che non si puo' usare e' il peggior esito possibile; un 'fail' per una preferenza estetica e' il secondo peggiore.
</output_format>$$,
true, 2, 'system', NOW())
ON CONFLICT (key) DO UPDATE SET
    content = EXCLUDED.content,
    version = EXCLUDED.version,
    updated_at = NOW(),
    updated_by = 'migration_0655';

-- ─────────────────────────────────────────────────────────────────────────────
-- (6) L'ultimo anello: chi IMPLEMENTA.
--
--     Senza questo la catena si spezza in fondo. La figura puo' scrivere il
--     requisito piu' preciso del mondo, ma chi scrive il codice deve sapere che
--     dichiarare uno stile e installarne la fonte sono DUE cose, e che la
--     seconda non avviene da sola. La direttiva <interfacce> esiste dalla mig
--     0650: qui si sostituisce il suo corpo, senza duplicarla.
--
--     Sostituzione ancorata all'intero blocco: cosi' ri-applicare la migrazione
--     non lo aggiunge una seconda volta, e un prompt che non lo contiene resta
--     intatto invece di ricevere un frammento a meta'.
-- ─────────────────────────────────────────────────────────────────────────────
UPDATE nexus_prompt_templates
   SET content = replace(
           content,
           '- se la richiesta indica uno stile, un riferimento o una libreria di componenti, quello vince sul catalogo.
</interfacce>',
           '- se la richiesta indica uno stile, un riferimento o una libreria di componenti, quello vince sul catalogo.
- lo stile va anche APPLICATO, non solo scritto: se usi le classi di un framework di utility, quel framework deve essere fra le dipendenze E configurato (file di configurazione o direttiva nel foglio che l''app carica). Altrimenti sono stringhe inerti e la pagina esce grezza mentre il codice sembra stilizzato. Vale allo stesso modo per un foglio di stile: se nessun modulo lo importa, non arriva mai al browser. Nel dubbio chiama ui_styling_audit, che risponde con un fatto;
- la scheda di ogni pattern ha una sezione `presentation` accanto a `structure`: dice che aspetto deve avere quel pattern (densita'', gerarchia del testo, comportamento a larghezza ridotta). Non e'' rifinitura facoltativa, e'' parte della scheda.
</interfacce>'
       ),
       updated_at = NOW(),
       updated_by = 'migration_0655'
 WHERE key IN ('system.nexus_base', 'agent.coder.base')
   AND content LIKE '%<interfacce>%'
   AND content NOT LIKE '%lo stile va anche APPLICATO%';
