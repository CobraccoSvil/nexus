#!/usr/bin/env bash
# scripts/gate-premesse.sh — Un gate che NON ha potuto misurare lo dichiara in un
# campo, e il testo lo compone chi conosce la causa (punto unico, regole L e Q).
#
# Da sorgere (`source`), non da eseguire. Lo sorge gate-env.sh, quindi ogni gate
# che gia' sorge quello lo ottiene; chi non passa da li' (precommit-turbo.sh) lo
# sorge direttamente.
#
# IL DIFETTO CHE CHIUDE (misurato l'08/08/2026, da due sessioni indipendenti).
#
#   In un worktree `D:\IDEAI-worktrees\*` manca il `.env` — sta solo nel repo
#   principale — quindi `precommit-cargo-check.sh` si fermava sul proprio
#   fail-closed prima di compilare qualunque cosa, e usciva con 1. Ma 1 e' anche
#   il codice con cui esce clippy quando trova un errore vero: due cause opposte,
#   lo stesso campo. lefthook, che vede solo "esito != 0", stampava il suo
#   `fail_text` statico:
#
#       pre-commit cargo fallito: correggi gli errori/warning clippy.
#
#   Non aveva compilato niente. Una sessione ha passato tempo a cercare un difetto
#   nel proprio codice; il solo segnale che diceva la verita' era la DURATA — 2,4s
#   contro i ~90s di un check reale (riprodotto qui: 233 ms). Cioe' l'unico modo di
#   sapere cosa fosse successo era cronometrare il gate.
#
# PERCHE' UN CODICE DEDICATO E NON UN MESSAGGIO MIGLIORE.
#
#   Il messaggio dello script c'era gia', ed era corretto: "DATABASE_URL non
#   impostato e non trovato in <repo>/.env". Non e' bastato, perche' sotto veniva
#   stampato un secondo messaggio che affermava il contrario con la stessa
#   autorevolezza. Finche' l'esito viaggia solo come prosa, chi sta a valle non ha
#   un campo da leggere e compone il testo che puo': quello statico (regola Q, lato
#   produttore — e' il produttore a decidere se il consumatore puo' rispettare la M).
#
#   Il campo, in shell, e' il codice d'uscita. 78 e' EX_CONFIG di sysexits.h:
#   convenzione riconosciuta, fuori dai codici che le shell si riservano (>125) e
#   distinta dall'1 generico. Da qui in poi "il gate non e' stato eseguito" e "il
#   gate ha bocciato il codice" sono due valori diversi, non due frasi diverse.
#
#   `fail_text` in lefthook resta statico per costruzione: non ha accesso
#   all'esito. Percio' nei comandi che possono fermarsi cosi' non afferma piu' una
#   causa — la dichiara il messaggio composto qui sotto, che la conosce.

# Il campo. I chiamanti e i test usano la costante, mai il letterale: cosi'
# cambiarlo e' una modifica sola, e un test che lo asserisce non fissa un numero
# scritto a mano in due posti.
export NEXUS_GATE_EXIT_CONFIG=78

# Il gate si ferma senza aver misurato nulla, e lo dice.
#
# Uso: gate_stop_configurazione "<causa in una riga>" "<dettaglio>" ...
# I dettagli sono righe libere (dove ha cercato, cosa manca, come si rimedia).
# Tutto su stderr: e' diagnostica, non il prodotto del gate.
gate_stop_configurazione() {
    local causa="$1"
    shift
    {
        echo ""
        echo "GATE NON ESEGUITO - configurazione mancante, non un difetto del codice."
        echo ""
        echo "  causa: $causa"
        local riga
        for riga in "$@"; do
            echo "  $riga"
        done
        echo ""
        echo "Nessuna verifica e' partita: questo esito non dice niente sul codice"
        echo "che stai committando. Il messaggio che segue, se c'e', e' il testo"
        echo "statico dell'hook e non conosce questa causa."
    } >&2
    exit "$NEXUS_GATE_EXIT_CONFIG"
}

# Premessa delle fasi TypeScript: turbo dev'essere invocabile in QUESTO albero.
#
# Stessa famiglia del `.env` mancante e stesso giorno: il worktree non ha
# `node_modules` (l'install pnpm vive nel repo principale), quindi `pnpm exec
# turbo` falliva con "Command not found" e il `fail_text` accusava typecheck/lint
# — cioe' del codice TypeScript che nessuno aveva letto.
#
# Si interroga lo STRUMENTO (`turbo --version`), non la presenza della directory
# `node_modules`: e' turbo che dev'essere invocabile, e un node_modules a meta'
# passerebbe un controllo sulla directory per fallire un attimo dopo.
#
# IL RIMEDIO E' INSTALLARE, NON CONDIVIDERE — e non e' una preferenza.
#
#   L'ipotesi naturale ("un link al node_modules del repo principale costa zero,
#   l'install costa 2 minuti per albero") e' stata provata e va SCARTATA. pnpm
#   materializza i pacchetti di workspace come symlink verso i sorgenti, e su
#   Windows li scrive ASSOLUTI. Misurato l'08/08/2026 nel repo principale:
#
#     node_modules/@ai-orchestrator/types -> D:/IDEAI/packages/types
#     node_modules/@ai-orchestrator/web-ide -> D:/IDEAI/apps/web-ide
#     apps/cli/node_modules/@ai-orchestrator/types -> D:/IDEAI/packages/types
#
#   Condividere quella directory con un worktree farebbe percio' risolvere ogni
#   dipendenza interna ai sorgenti del REPO PRINCIPALE: il typecheck girerebbe
#   nell'albero giusto e compilerebbe i file di un altro, restando verde. E' il
#   falso verde della regola O nella forma peggiore — silenzioso — e per di piu'
#   nel punto che `hook_tree_guard` presidia (i gate misurano l'albero che
#   committa), aggirandolo dal basso.
#
#   Con `pnpm install` eseguito NEL worktree gli stessi link puntano al worktree
#   (verificato su un albero che l'aveva fatto): la separazione la produce
#   l'install, e nient'altro. I 2 minuti sono il prezzo della misura giusta.
#
#   Un secondo motivo, indipendente: pnpm SCRIVE dentro node_modules. Un albero
#   che ci installasse attraverso un link modificherebbe l'installazione di tutti
#   gli altri.
gate_pretende_turbo() {
    if pnpm exec turbo --version >/dev/null 2>&1; then
        return 0
    fi
    gate_stop_configurazione \
        "turbo non e' invocabile in questo albero (dipendenze node non installate)." \
        "albero: ${NEXUS_GATE_ROOT:-$(pwd)}" \
        "" \
        "Rimedio: 'pnpm install --frozen-lockfile' in QUESTO albero (~2 min)." \
        "Ogni albero ha il proprio node_modules: i link di workspace che pnpm" \
        "materializza sono ASSOLUTI, quindi un node_modules condiviso farebbe" \
        "compilare i sorgenti dell'ALTRO albero (regola O)."
}
