-- Chiusura lato SERVER delle sessioni inattive del ruolo applicativo.
--
-- Il tetto per pool e il registro unico dei pool (nexus-project-pools) governano
-- le connessioni finche' il processo che le possiede e' vivo: l'idle_timeout di
-- sqlx e' un timer che vive DENTRO quel processo. Quando il processo muore
-- lasciando i socket aperti -- riavvio dei servizi, crash, deploy -- nessun
-- timer client puo' piu' agire, e Postgres non se ne accorge: le sessioni
-- restano 'idle' e continuano a occupare slot del rolconnlimit.
--
-- Misurato il 2026-07-22 sul cluster app: 51 socket verso :5434, di cui 41
-- appartenenti a QUATTRO processi non piu' esistenti, alcuni idle da 9,7 ore.
-- Il ruolo nexus_app era a 50 connessioni su un limite di 50: qualunque
-- apertura di pool falliva e il sistema era fermo per intero.
--
-- Il ruolo aveva gia' statement_timeout e idle_in_transaction_session_timeout,
-- ma nessuno dei due tocca lo stato 'idle' fuori transazione, che e' esattamente
-- quello in cui restano le sessioni orfane. idle_session_timeout e' il termine
-- che mancava (Postgres 14+, qui 17.10).
--
-- Il valore e' molto piu' alto dell'idle_timeout client (60s): in condizioni
-- normali chiude sempre prima il client, e questo resta il backstop per le
-- sessioni che nessun client puo' piu' chiudere. sqlx verifica la connessione
-- prima di consegnarla, quindi una sessione chiusa dal server non produce
-- errori applicativi: il pool ne apre un'altra.
--
-- Perche' qui: il ruolo del cluster app e' configurato fuori dal repo (come il
-- rolconnlimit), e questo e' l'unico set di migrazioni che gira su quel cluster.
-- L'effetto e' cluster-wide e idempotente: rieseguirlo su ogni DB-progetto
-- riscrive lo stesso valore. Cosi' la difesa e' versionata (regola H) e vale
-- anche per i progetti gia' esistenti, al primo accesso dopo il deploy.
--
-- Su current_user e non su un nome fisso: il ruolo che apre i pool e' quello che
-- esegue questa migrazione, qualunque sia (niente nomi hardcoded, regola G).

DO $$
BEGIN
    EXECUTE format(
        'ALTER ROLE %I SET idle_session_timeout = %L',
        current_user,
        '10min'
    );
EXCEPTION
    WHEN insufficient_privilege THEN
        -- Un ruolo puo' impostare i propri parametri USERSET; se una policy del
        -- cluster lo impedisse, la migrazione NON deve bloccare lo schema del
        -- progetto: il difetto sarebbe peggiore del rimedio.
        RAISE NOTICE
            'idle_session_timeout non impostato su %: privilegi insufficienti. Impostarlo sul ruolo a livello di cluster.',
            current_user;
END
$$;
