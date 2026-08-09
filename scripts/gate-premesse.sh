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

# Premessa delle fasi che eseguono JavaScript: il Node in esecuzione deve
# soddisfare il minimo che il repo dichiara in package.json (`engines.node`).
#
# IL DIFETTO CHE CHIUDE, e perche' e' della stessa famiglia dei due sopra.
#
#   `.github/workflows/verify.yml` dichiarava `node-version: "20"` dal
#   2026-06-15 (commit a7c7fa68). Il 2026-07-03 (commit 81d1b921) i test della
#   web-ide sono diventati `node --test "lib/**/*.test.ts"`: file TypeScript
#   eseguiti col type-stripping nativo, elencati da un glob. Node 20 non sa fare
#   ne' l'uno ne' l'altro. Da quel giorno il gate CI e' morto SEMPRE nello stesso
#   punto, e mai una volta ha eseguito clippy o nextest — che nel gate vengono
#   dopo. Cioe' le fasi Rust in CI non giravano da oltre un mese.
#
#   MISURATO in locale l'08/08/2026, lo stesso comando dello script `test`:
#     node 20.19.5 : Could not find '<...>/lib/**/*.test.ts'   (glob non espanso)
#     node 22.11.0 : glob OK, ERR_UNKNOWN_FILE_EXTENSION ".ts" (niente stripping)
#     node 22.18.0 : 105 pass, 0 fail                          (primo che regge)
#     node 24.14.0 : 229 pass, 0 fail                          (versione locale)
#
#   Il messaggio che il CI stampava — "Could not find lib/**/*.test.ts" — accusa
#   un percorso mancante: chi lo legge cerca un file, non una versione di Node.
#   E' lo stesso schema del `.env` e di `node_modules`: una premessa non
#   soddisfatta che si presenta come un difetto del codice, perche' nessuno la
#   pretendeva prima di misurare.
#
# LA VERSIONE HA ORA UN PUNTO UNICO (regola L): `.nvmrc` dichiara quella USATA
# — la leggono i workflow via `node-version-file` e nvm in locale — e
# `engines.node` il MINIMO tollerato, che e' cio' che questa premessa verifica.
# Prima la versione era scritta a mano in due workflow e in nessun altro posto:
# il locale girava su 24 e la CI su 20 senza che niente dichiarasse la
# differenza.
#
# Forma ammessa per `engines.node`: `>=X.Y.Z`. Un range piu' ricco (unioni,
# caret) richiederebbe un parser semver, che qui sarebbe una seconda verita'
# rispetto a quello di npm; se il campo non e' in questa forma il gate si ferma
# dicendolo, invece di indovinare un confronto.
gate_pretende_node() {
    local root="${NEXUS_GATE_ROOT:-$(pwd)}"

    if ! command -v node >/dev/null 2>&1; then
        gate_stop_configurazione \
            "node non e' nel PATH: nessuna fase JavaScript del gate e' eseguibile." \
            "albero: ${root}" \
            "" \
            "Rimedio: installa Node. La versione da usare la dichiara ${root}/.nvmrc" \
            "(nvm: 'nvm use'; CI: node-version-file nei workflow)."
    fi

    local minimo corrente
    # `./package.json` relativo alla cwd, non un path assoluto interpolato nel
    # sorgente JS: NEXUS_GATE_ROOT e' in forma POSIX anche su Windows e
    # `require()` non la risolve.
    minimo="$(cd "$root" && node -p "require('./package.json').engines?.node ?? ''" 2>/dev/null || true)"
    corrente="$(node -p 'process.versions.node' 2>/dev/null || true)"

    # Nessun minimo dichiarato: non c'e' niente da pretendere. Non si inventa un
    # valore di ripiego (regola G): un default nascosto renderebbe il gate
    # silenziosamente piu' permissivo il giorno in cui il campo sparisse.
    [[ -n "$minimo" ]] || return 0

    if [[ ! "$minimo" =~ ^\>=[0-9]+\.[0-9]+\.[0-9]+$ ]]; then
        gate_stop_configurazione \
            "engines.node vale '${minimo}', che questo gate non sa confrontare." \
            "forma ammessa: '>=X.Y.Z' (vedi il commento in scripts/gate-premesse.sh)" \
            "" \
            "Rimedio: riportalo a quella forma in ${root}/package.json, oppure" \
            "insegna il range nuovo a gate_pretende_node."
    fi

    if node -e '
        const [min, cur] = process.argv.slice(1);
        const parti = (s) => {
            const n = s.replace(/^[^0-9]*/, "").split(".").map((x) => Number(x) || 0);
            return [n[0] || 0, n[1] || 0, n[2] || 0];
        };
        const [a, b, c] = parti(min);
        const [x, y, z] = parti(cur);
        process.exit(x > a || (x === a && (y > b || (y === b && z >= c))) ? 0 : 1);
    ' "$minimo" "$corrente"; then
        return 0
    fi

    gate_stop_configurazione \
        "node ${corrente} non soddisfa il minimo dichiarato dal repo (${minimo})." \
        "albero: ${root}" \
        "" \
        "Cosa si romperebbe senza questo stop: i test della web-ide sono file" \
        ".ts eseguiti da 'node --test' col type-stripping nativo ed elencati da" \
        "un glob. Sotto 22.18.0 Node non fa ne' l'uno ne' l'altro, e il rosso che" \
        "ne esce dice \"Could not find lib/**/*.test.ts\": accusa un percorso" \
        "mancante, non la versione." \
        "" \
        "Rimedio: usa la versione dichiarata in ${root}/.nvmrc ('nvm use')."
}

# Premessa delle fasi di test Rust: l'esecutore del gate e' cargo-nextest.
#
# Sta qui, e con il codice 78, per la ragione di questo file: "l'esecutore dei
# test non e' installato" e "i test sono rossi" sono due cause opposte, e finche'
# uscivano entrambe come 1 il consumatore non aveva un campo da leggere. Il
# messaggio c'era gia' ed era corretto (verify.sh lo stampava prima di
# `exit 1`), ma valeva quanto quello del `.env`: sopra ne veniva stampato un
# altro, statico, che affermava il contrario con la stessa autorevolezza.
#
# Il ripiego su `cargo test` resta escluso: locale e CI devono misurare la
# stessa cosa, e nextest non esegue i doctest — un gate che ripiegasse
# cambierebbe copertura senza dirlo (regola O).
gate_pretende_nextest() {
    if cargo nextest --version >/dev/null 2>&1; then
        return 0
    fi
    gate_stop_configurazione \
        "cargo-nextest non e' installato: e' l'esecutore dei test del gate." \
        "albero: ${NEXUS_GATE_ROOT:-$(pwd)}" \
        "" \
        "Rimedio: 'cargo install cargo-nextest --locked'." \
        "In CI lo installa taiki-e/install-action@nextest (binario precompilato)." \
        "Nessun ripiego su 'cargo test': misurerebbe una cosa diversa dalla CI."
}
