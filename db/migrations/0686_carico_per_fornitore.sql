-- 0686 — Le convocazioni parallele guardavano lo STATO del fornitore, mai il suo CARICO.
--
-- ROOT CAUSE (misurata l'08/08/2026 su gestione-corsi). Il Consiglio ha
-- convocato otto figure in fan-out — provider_analyst (due), program_manager,
-- project_manager, functional_analyst, security_engineer, software_architect,
-- ui_ux_designer — e sono scadute TUTTE E OTTO. Zero pareri su otto, budget
-- consumato per intero, nessun prodotto. Nello stesso momento tre fornitori su
-- nove erano fuori per credito esaurito (anthropic, openai, perplexity) e ne
-- restavano cinque a servire otto chiamate concorrenti, piu' il carico di
-- quattro sessioni di sviluppo in parallelo.
--
-- Il tempo NON era latenza del modello: era tempo di CODA. Il fornitore
-- rispondeva bene, semplicemente non a otto chiamate insieme — quindi un
-- timeout adattivo sulla latenza avrebbe misurato la cosa sbagliata, e alzare
-- il tetto per figura sarebbe stata la toppa che la regola H vieta (il tetto e'
-- gia' corretto dal fix 146129d2 del 22/07, che lo deriva dal timeout REALE
-- della figura).
--
-- LA DOMANDA CHE NON ESISTEVA. Il sistema sapeva rispondere sullo STATO di un
-- fornitore — `nexus_provider_health` (ha credito?), `provider_cooldown` (e'
-- escluso adesso?, e dalla mig 0683 con la portata giusta) — e nessuna di quelle
-- domande cambia risposta perche' altre sette chiamate sono partite un
-- millisecondo fa. Otto figure sceglievano fra gli stessi cinque fornitori, e
-- nessuna sapeva delle altre sette.
--
-- Due semafori esistevano gia' e NON potevano coprire il caso, perche'
-- governano il NUMERO e mai la DESTINAZIONE:
--   orchestrator.subagent_fanout_max_parallel  (locale al fan-out, default 6)
--   orchestrator.fanout_process_max_parallel   (di processo,      default 12)
-- Dodici chiamate concorrenti tutte verso lo stesso fornitore rispettano
-- entrambi i tetti.
--
-- LA FORMA CONCRETA DELLA CONCENTRAZIONE. `resolve_council_assignments` assegna
-- alle figure fornitori DISTINTI escludendo via via quelli gia' usati. Con otto
-- figure e cinque fornitori le prime cinque ne prendevano uno ciascuno, e dalla
-- sesta l'esclusione svuotava il pool: il ripiego era `resolve_purpose_model_db`
-- SENZA esclusione, cioe' «il piu' preferito», identico per tutte e tre le
-- eccedenti. Un fornitore si ritrovava quattro chiamate, gli altri quattro una.
--
-- COSA CAMBIA. Nasce il punto unico `mcp-core/src/provider_inflight.rs` (regola
-- L) che sa quante chiamate sono in volo verso ciascuna coppia
-- (fornitore, modello): il conteggio lo tiene una guardia RAII, cosi' un task
-- cancellato — cioe' ogni figura che scade — decrementa nel Drop invece di
-- lasciare il fornitore eternamente saturo proprio dopo un'ondata di timeout.
-- Da quel registro derivano due cose: una CODA per fornitore, e il criterio con
-- cui il ripiego sopra distribuisce i duplicati inevitabili invece di
-- ammucchiarli.
--
-- IL TETTO E' DI SCHEDULING, NON DI AMMISSIONE. Allo scadere dell'attesa la
-- chiamata parte comunque, dichiarandolo: rifiutarla trasformerebbe un ritardo
-- in un fallimento certo, e la misura di successo e' «pareri invece di timeout»,
-- non «code piu' ordinate».
--
-- Il fan-out NON si riduce: convocare cinque figure invece di otto comprerebbe
-- la puntualita' con la qualita' del parere, e il Consiglio vale perche' sono
-- punti di vista diversi. `resolve_orchestration_plan` resta intoccato.

INSERT INTO settings (key, value, description, category)
VALUES (
    'routing.inflight_max_per_provider',
    '3',
    'Quante chiamate concorrenti verso UNO stesso fornitore prima di accodare le '
    'successive. Non e'' una quota: allo scadere dell''attesa la chiamata parte '
    'comunque (tetto di scheduling, vedi routing.inflight_queue_wait_max_s). Tre e'' '
    'il punto in cui, con cinque fornitori disponibili, un fan-out da otto viene '
    'servito senza che nessuno ne riceva piu'' di tre insieme. Il valore si applica '
    'al riavvio: un semaforo non si rimpicciolisce a caldo senza stati transitori.',
    'routing'
),
(
    'routing.inflight_queue_wait_max_s',
    '90',
    'Quanto si attende al massimo il proprio turno su un fornitore saturo. Scaduto, '
    'la chiamata parte lo stesso e l''attesa viene DICHIARATA: se il run poi muore '
    'senza mai un turno, la causa del timeout dice "queued_never_ran" invece di '
    'mandare a cercare un modello lento. Generoso di proposito: un''attesa piu'' corta '
    'della durata di una chiamata equivarrebbe a non accodare affatto.',
    'routing'
)
ON CONFLICT (key) DO UPDATE
    SET value = EXCLUDED.value,
        description = EXCLUDED.description;
