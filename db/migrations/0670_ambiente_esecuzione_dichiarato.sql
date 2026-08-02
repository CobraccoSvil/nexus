-- 0670_ambiente_esecuzione_dichiarato.sql
-- L'agente scopriva per tentativi su quale host stava girando.
--
-- SINTOMO OSSERVATO (02/08/2026, progetto bacheca-attivita). Il sub-run
-- a5f7419c, kind 'verify', ha chiuso in 'timeout' dopo 180s esatti (il tetto
-- della sua definition, il piu' corto del parco figure) e 16 iterazioni, senza
-- consegnare nulla. La coda della sua storia in agent_steps:
--
--   iter 12: `which jq`            -> EXIT CODE 1
--            "which: no jq in (/mingw64/bin:/usr/bin:...)"
--   iter 15:                        -> EXIT CODE 0, "jq is NOT installed"
--   iter 16: `sudo apt-get update` -> "[sudo] apt-get update fallito:
--            binary nexus-sudo-runner non trovato in /usr/..."
--
-- Il PATH della prima riga dice gia' che siamo in Git Bash su Windows. La figura
-- lo stava scoprendo a proprie spese, un'ipotesi per iterazione, mentre il
-- sistema quel fatto lo aveva in mano dall'avvio.
--
-- CAUSA, in due parti.
--
-- (1) L'ambiente reale non entrava in NESSUN contesto. Il CLAUDE.md del repo
--     dichiara l'ambiente canonico ("Windows nativo, PowerShell, niente WSL"),
--     ma e' un documento per gli umani: non e' un dato che il sistema legge, e
--     descrive il repo Nexus, non l'host di un progetto utente. Cio' che il
--     sistema SA davvero -- il sistema operativo del processo, la shell con cui
--     `run_command` esegue (nexus_tool_kit::sandbox::agent_shell, che su Windows
--     e' Git Bash), quali gestori di pacchetti rispondono nel PATH -- non
--     arrivava all'agente in nessuna forma.
--
-- (2) Peggio dell'assenza: il system prompt lo mandava DALLA PARTE SBAGLIATA. Il
--     blocco <privilegi_sistema> di system.nexus_base afferma, verbatim:
--
--       "Puoi installare dipendenze di SISTEMA che richiedono privilegi di root
--        [...] eseguendo il comando con run_command: sudo apt-get install -y"
--
--     Scritto quando Nexus girava su un host Linux. Su Windows non e'
--     un'omissione: e' un'affermazione falsa con l'autorita' del system prompt,
--     e manda l'agente contro un muro che nessuna iterazione puo' aggirare --
--     il gestore privilegiato (nexus-sudo-runner, ADR 0017) e' un binario Linux
--     e li' non esiste.
--
-- IL FIX NON E' UN FLAG DI PIATTAFORMA. Sistema operativo, shell e gestori
-- installati non sono configurazione: sono cio' che c'e', e si MISURANO. Un
-- settings.agent.platform='windows' sarebbe una seconda verita' da tenere
-- allineata a mano, e la prima volta che divergesse mentirebbe con l'aria di una
-- configurazione. Il rilevamento vive nel punto unico
-- `nexus-agent-tools::ambiente` e raggiunge la shell per la stessa strada dei
-- comandi dell'agente (regola O); l'innesto nei due compositori di system prompt
-- (chat e sub-run) e' `mcp-core::prompt_ambiente`.
--
-- QUI STA SOLO IL VOCABOLARIO (regola G): QUALI nomi sondare, e quale gestore la
-- direttiva privilegiata presuppone. La regola -- come si sonda, cosa si
-- dichiara, quando si toglie la direttiva -- resta nel codice. La domanda posta
-- all'host e' generale (regola H): non "siamo su Windows?" -- Windows e'
-- un'istanza, e inseguire le piattaforme a codice e' la toppa -- ma "questo nome
-- risponde nel PATH?". Un gestore nuovo e' una riga qui, non un deploy.
--
-- SULL'ELENCO. Sono sondati TUTTI i nomi su qualunque host: il costo e' una
-- ricerca in-process nel PATH, con cache a 300s, e l'esito e' proprio cio' che
-- serve dichiarare -- su Windows `apt-get` risultera' assente, ed e' quella riga
-- a chiudere il giro di tentativi. Un elenco filtrato per sistema operativo
-- sarebbe una variante a codice travestita da dato.

INSERT INTO settings (key, value, description, category)
VALUES (
    'agent.environment.package_managers',
    'apt-get,apt,dnf,yum,pacman,apk,brew,winget,choco,scoop',
    'Gestori di pacchetti che il rilevamento d''ambiente sonda nel PATH dell''host. '
    'Il blocco <ambiente_esecuzione> del system prompt dichiara quali rispondono e '
    'quali NO: e'' l''elenco degli assenti a impedire i tentativi a vuoto. '
    'Vuoto o assente -> il blocco dichiara di non aver guardato (mai "nessuno '
    'disponibile", che avrebbe l''aria di una rilevazione).',
    'agent'
)
ON CONFLICT (key) DO NOTHING;

INSERT INTO settings (key, value, description, category)
VALUES (
    'agent.environment.privileged_install_manager',
    'apt-get',
    'Gestore che il blocco <privilegi_sistema> di system.nexus_base PRESUPPONE. '
    'Se quel nome risulta ASSENTE sull''host, la direttiva viene rimossa dal system '
    'prompt: afferma una capacita'' che li'' non esiste. Rimozione solo su assenza '
    'ACCERTATA -- un PATH illeggibile o un nome fuori dal vocabolario sopra non '
    'sono una prova, e il prompt resta intatto.',
    'agent'
)
ON CONFLICT (key) DO NOTHING;
