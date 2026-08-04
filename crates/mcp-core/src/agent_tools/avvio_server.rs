//! Punto unico (regola L): «questa riga di shell AVVIA un server?».
//!
//! La risposta governa tre poteri concessi a t=0, prima che il processo esista:
//! l'allocazione della porta iniettata come `PORT`, l'attesa dell'ascolto come
//! prova d'avvio, e — per il tramite della label — il potere di FERMARE i
//! servizi simili gia' vivi (`gate_pre_avvio` -> `dedup_and_cleanup_ports` ->
//! `stop_similar_running_services`, invocato PRIMA dello spawn).
//!
//! ROOT CAUSE, misurata il 03/08/2026 sul parco progetti. Il criterio era
//! `command.to_lowercase().contains(token)` su un vocabolario di sottostringhe,
//! e `contains` non distingue un comando che NOMINA un server da uno che lo
//! ESEGUE. Il token nudo `vite` rendeva «servizio web» la stringa
//! `VITE_API_URL`, quindi:
//!
//! | vittima                | vissuta | uccisa da                              |
//! |------------------------|---------|----------------------------------------|
//! | `vite --port 24804`    | 3h 45m  | `grep -r "VITE_API_URL" frontend/`     |
//! | `vite --port 24804`    | 2h 05m  | `node -e "const fs=require('fs');..."` |
//! | servizio frontend      | 18m     | `node_modules/.bin/vite --version`     |
//! | servizio frontend      | 14m     | `cd /d ...\catalogo-libri && dir`      |
//! | servizio frontend      | 11m     | `ls node_modules/.pnpm \| grep esbuild`|
//!
//! Un `grep`, un `dir`, un `ls` e un `--version` fermavano servizi vivi da ore:
//! riconosciuti come servizi, ricevevano dalla working directory la label
//! `frontend`, e il gate pre-avvio fermava il frontend vero per «deduplicarlo».
//!
//! Il difetto NON era quale lista fosse autoritativa — inseguirne le voci e' la
//! toppa che la regola H vieta, e il vocabolario porta gia' nel codice due
//! accrescimenti per incidente. Il difetto era la FORMA della domanda. Qui la
//! riga viene scomposta in comandi e parole dal punto unico
//! [`super::playwright_cli::comandi`] (virgolette risolte, separatori `&&`,
//! `||`, `;`, `|`, `&`, assegnazioni env inline separate) e la domanda diventa
//! «l'ESEGUIBILE di uno di questi comandi avvia un server?».
//!
//! E' la stessa correzione gia' fatta una volta in questo albero, per la stessa
//! forma di difetto: `playwright_cli` nasce perche' `command.contains("playwright")`
//! registrava un test per `cat playwright.config.ts`. Qui il testo E' l'oggetto
//! — una command line e' un dato sintattico — quindi scomporla non viola la
//! regola M, che vieta di dedurre lo STATO di una richiesta dalla prosa.
//!
//! Cio' che questo modulo NON fa: accertare che il servizio serva davvero.
//! Quello e' un fatto osservabile e arriva dopo (`attende_ascolto`); qui si
//! decidono i POTERI del lancio, e un potere si concede prima di eseguire.

use super::playwright_cli::{comandi, Comando};

/// Come un eseguibile si rapporta ai propri sottocomandi, per la sola domanda
/// «avvia un server?».
///
/// Esiste come tipo perche' le tre risposte non sono la stessa cosa detta in
/// modi diversi, e la differenza e' misurabile: con un unico insieme piatto
/// `vite build` diventerebbe un servizio (il token `vite` basta), mentre
/// `gunicorn app:api` smetterebbe di esserlo (`app:api` non e' un sottocomando).
enum Natura {
    /// L'eseguibile basta: ogni argomento e' configurazione, non un verbo.
    /// `gunicorn app:api`, `uvicorn main:app --reload`.
    Sempre,
    /// Avvia un server SOLO con certi sottocomandi: `next dev`, `cargo run`.
    /// Nudo non fa nulla di utile, o fa altro.
    SoloCon(&'static [&'static str]),
    /// Nudo e' un server, ma un sottocomando esplicito puo' negarlo:
    /// `vite` e `vite dev` servono, `vite build` compila.
    SalvoCon(&'static [&'static str]),
}

/// Vocabolario degli eseguibili che avviano un server.
///
/// Resta un vocabolario, e non puo' non esserlo: «avvia un server» non e'
/// deducibile dal nome di un binario arbitrario. Ma e' un vocabolario di
/// ESEGUIBILI, chiuso e verificabile, non di sottostringhe che possono
/// comparire ovunque in una riga — in un percorso, in una variabile
/// d'ambiente, dentro una stringa passata a `grep`.
const SERVER: &[(&str, Natura)] = &[
    // Bundler e dev server con eseguibile proprio.
    ("vite", Natura::SalvoCon(&["dev", "preview", "serve"])),
    ("webpack-dev-server", Natura::Sempre),
    ("webpack", Natura::SoloCon(&["serve"])),
    ("next", Natura::SoloCon(&["dev", "start"])),
    ("nuxt", Natura::SoloCon(&["dev", "start"])),
    ("astro", Natura::SoloCon(&["dev", "start", "preview"])),
    ("svelte-kit", Natura::SoloCon(&["dev"])),
    ("ng", Natura::SoloCon(&["serve"])),
    ("react-scripts", Natura::SoloCon(&["start"])),
    ("expo", Natura::SoloCon(&["start"])),
    ("remix", Natura::SoloCon(&["dev"])),
    ("parcel", Natura::SoloCon(&["serve"])),
    // Server statici.
    ("http-server", Natura::Sempre),
    ("live-server", Natura::Sempre),
    ("browser-sync", Natura::Sempre),
    ("serve", Natura::Sempre),
    // Python.
    ("gunicorn", Natura::Sempre),
    ("uvicorn", Natura::Sempre),
    ("hypercorn", Natura::Sempre),
    ("daphne", Natura::Sempre),
    ("flask", Natura::SoloCon(&["run"])),
    ("django-admin", Natura::SoloCon(&["runserver"])),
    ("fastapi", Natura::SoloCon(&["dev", "run"])),
    // Ruby.
    ("rails", Natura::SoloCon(&["server", "s"])),
    ("puma", Natura::Sempre),
    ("unicorn", Natura::Sempre),
    // Rust / Go / .NET / Java.
    ("cargo", Natura::SoloCon(&["run", "watch"])),
    ("go", Natura::SoloCon(&["run"])),
    ("air", Natura::Sempre),
    ("dotnet", Natura::SoloCon(&["run", "watch"])),
    ("gradlew", Natura::SoloCon(&["bootrun"])),
    // Runtime JS coi propri sottocomandi; quando invece eseguono uno SCRIPT
    // passato come percorso, la risposta la da' il file (vedi [`ENTRYPOINT_SERVER`]).
    ("deno", Natura::SoloCon(&["serve", "task"])),
    ("nodemon", Natura::Sempre),
    ("ts-node-dev", Natura::Sempre),
    ("concurrently", Natura::Sempre),
];

/// Runtime che eseguono uno SCRIPT passato come percorso: per questi il nome
/// dell'eseguibile non basta (`node` esegue un server come esegue un `-e` di
/// tre righe), e la domanda si sposta sul file.
const RUNTIME_CON_SCRIPT: &[&str] = &["node", "ts-node", "tsx", "bun", "deno", "nodemon"];

/// Nomi di entrypoint che per convenzione avviano un server.
const ENTRYPOINT_SERVER: &[&str] = &["server", "app", "index", "main"];

/// Estensioni di uno script eseguibile da un runtime JS.
const ESTENSIONI_SCRIPT: &[&str] = &["js", "mjs", "cjs", "ts", "mts", "cts"];

/// Script di `package.json` che, per convenzione universale, avviano un server.
///
/// Non e' un'euristica sul nome del progetto: e' il contratto che npm, pnpm e
/// yarn documentano e che ogni scaffolding genera.
const SCRIPT_SERVER: &[&str] = &["dev", "start", "serve"];

/// Gestori di pacchetti che eseguono uno script o un binario locale.
///
/// `deno` NON e' qui pur potendo lanciare: ha sottocomandi propri che avviano
/// un server (`deno serve`, `deno task`), e trattarlo da lanciatore faceva
/// passare `task` per l'eseguibile — che non e' in nessun vocabolario.
const LANCIATORI: &[&str] = &["npx", "npm", "pnpm", "yarn", "bun", "bunx"];

/// Sottocomandi di un lanciatore che precedono ancora cio' che viene eseguito.
const PONTI_LANCIATORE: &[&str] = &["run", "exec", "dlx", "x"];

/// Flag che chiedono un'informazione e non avviano MAI nulla, per nessun
/// eseguibile. Universali: non sono una lista di istanze da inseguire.
///
/// Misurato: `node_modules/.bin/vite --version` ha fermato un servizio vivo da
/// 18 minuti.
const FLAG_INFORMATIVI: &[&str] = &["--version", "-v", "-V", "--help", "-h", "help"];

/// Basename dell'eseguibile: senza directory, estensione Windows e `@versione`.
fn eseguibile(parola: &str) -> String {
    let base = parola
        .rsplit(['/', '\\'])
        .next()
        .unwrap_or(parola)
        .to_ascii_lowercase();
    // `pkg@1.2.3` -> `pkg`, ma solo se non e' uno scoped package (`@scope/pkg`,
    // gia' ridotto a `pkg` dallo split sul separatore di percorso).
    let base = match base.strip_prefix('@') {
        Some(_) => base.clone(),
        None => base.split('@').next().unwrap_or(&base).to_string(),
    };
    for ext in [".cmd", ".exe", ".ps1", ".bat"] {
        if let Some(s) = base.strip_suffix(ext) {
            return s.to_string();
        }
    }
    base
}

/// Salta lanciatori, ponti (`run`, `exec`) e i loro flag, restituendo l'indice
/// della parola che identifica cio' che viene davvero eseguito.
///
/// `pnpm --dir ./backend dev` -> `dev`; `npx vite --port 3000` -> `vite`.
/// I flag del LANCIATORE (`--dir`, `--filter`, `-C`) si saltano insieme al loro
/// valore quando lo portano separato: senza, `--dir ./backend` farebbe passare
/// `./backend` per l'eseguibile.
fn indice_eseguito(parole: &[String]) -> Option<usize> {
    let mut i = 0;
    let mut visto_lanciatore = false;
    while i < parole.len() {
        let p = parole[i].to_ascii_lowercase();
        let exe = eseguibile(&p);
        if LANCIATORI.contains(&exe.as_str()) {
            visto_lanciatore = true;
            i += 1;
            continue;
        }
        if visto_lanciatore && PONTI_LANCIATORE.contains(&p.as_str()) {
            i += 1;
            continue;
        }
        if p.starts_with('-') {
            // Flag del lanciatore col valore separato (`--dir ./backend`).
            let porta_valore = visto_lanciatore
                && !p.contains('=')
                && matches!(
                    p.as_str(),
                    "--dir" | "--filter" | "-f" | "-c" | "--prefix" | "--workspace" | "-w"
                );
            i += if porta_valore { 2 } else { 1 };
            continue;
        }
        return Some(i);
    }
    None
}

/// Il comando chiede solo un'informazione (`--version`, `--help`)?
fn solo_informativo(parole: &[String]) -> bool {
    parole
        .iter()
        .any(|p| FLAG_INFORMATIVI.contains(&p.to_ascii_lowercase().as_str()))
}

/// Lo script che il runtime esegue e' un entrypoint di server?
///
/// Guarda il primo argomento non-flag dopo il runtime. Un `-e` inline non ha
/// script (`node -e "..."` e' un programma qualunque scritto sulla riga), e uno
/// script senza estensione non e' un file: sono i due modi in cui un `node`
/// veniva scambiato per un servizio.
fn script_e_un_server(parole: &[String], runtime: usize) -> bool {
    for arg in parole.iter().skip(runtime + 1) {
        // `deno run server.ts`, `bun run app.js`: il ponte precede lo script.
        if PONTI_LANCIATORE.contains(&arg.to_ascii_lowercase().as_str()) {
            continue;
        }
        if arg.starts_with('-') {
            // `node -e "..."`, `node --experimental-vm-modules ...`: il valore
            // di `-e`/`--eval` e' codice, non un percorso.
            if matches!(arg.as_str(), "-e" | "--eval" | "-p" | "--print") {
                return false;
            }
            continue;
        }
        let file = arg
            .rsplit(['/', '\\'])
            .next()
            .unwrap_or(arg)
            .to_ascii_lowercase();
        let Some((nome, estensione)) = file.rsplit_once('.') else {
            // Senza estensione non e' uno script: `deno serve`, `bun run dev`
            // sono gia' coperti dai sottocomandi.
            return false;
        };
        return ENTRYPOINT_SERVER.contains(&nome) && ESTENSIONI_SCRIPT.contains(&estensione);
    }
    false
}

/// Sottocomando dell'eseguibile: la parola IMMEDIATAMENTE successiva, e solo
/// se non e' un flag.
///
/// Non «il primo argomento non-flag»: quello prende il VALORE di un flag.
/// `vite --port 24804` avrebbe come sottocomando `24804`, che nessun
/// vocabolario ammette, e il dev server piu' comune del parco non sarebbe piu'
/// stato riconosciuto. Nella convenzione di ogni CLI il verbo precede le
/// opzioni: dopo un flag c'e' configurazione, non un verbo.
fn sottocomando(parole: &[String], dopo: usize) -> Option<String> {
    parole
        .get(dopo + 1)
        .filter(|p| !p.starts_with('-'))
        .map(|p| p.to_ascii_lowercase())
}

/// Un singolo comando semplice avvia un server?
fn comando_avvia_server(cmd: &Comando) -> bool {
    if cmd.parole.is_empty() || solo_informativo(&cmd.parole) {
        return false;
    }
    // Un runtime che esegue uno SCRIPT si giudica per primo, e sul comando
    // nudo: `bun` e `deno` sono insieme lanciatori (`bun run dev`) e runtime
    // (`bun src/index.ts`), e saltarli come lanciatori toglieva di mezzo
    // proprio lo script su cui va posta la domanda.
    let exe_nudo = cmd.parole.first().map(|p| eseguibile(p)).unwrap_or_default();
    if RUNTIME_CON_SCRIPT.contains(&exe_nudo.as_str()) && script_e_un_server(&cmd.parole, 0) {
        return true;
    }

    let Some(i) = indice_eseguito(&cmd.parole) else {
        return false;
    };
    let parola = &cmd.parole[i];
    let exe = eseguibile(parola);

    // Uno script di package.json (`npm run dev`, `pnpm dev`): la parola
    // eseguita non e' un binario ma il nome dello script.
    let lanciato = cmd
        .parole
        .first()
        .map(|p| LANCIATORI.contains(&eseguibile(p).as_str()))
        .unwrap_or(false);
    if lanciato && SCRIPT_SERVER.contains(&exe.as_str()) {
        return true;
    }

    if let Some((_, natura)) = SERVER.iter().find(|(nome, _)| *nome == exe) {
        let sotto = sottocomando(&cmd.parole, i);
        return match natura {
            Natura::Sempre => true,
            Natura::SoloCon(ammessi) => sotto
                .as_deref()
                .is_some_and(|s| ammessi.contains(&s)),
            Natura::SalvoCon(ammessi) => match sotto.as_deref() {
                None => true,
                Some(s) => ammessi.contains(&s),
            },
        };
    }

    // `php -S localhost:8000` e `python manage.py runserver`: il verbo non e'
    // l'eseguibile ma un suo argomento, e senza quello il runtime non serve
    // nulla.
    match exe.as_str() {
        "php" => cmd.parole.iter().any(|p| p == "-S"),
        "python" | "python3" | "py" => {
            let ha = |t: &str| cmd.parole.iter().any(|p| p.eq_ignore_ascii_case(t));
            ha("runserver")
                || (ha("-m")
                    && cmd.parole.iter().any(|p| {
                        let m = p.to_ascii_lowercase();
                        m == "uvicorn" || m == "gunicorn" || m == "hypercorn" || m == "http.server"
                    }))
        }
        "ruby" => cmd.parole.iter().any(|p| p == "httpd"),
        "java" => cmd.parole.iter().any(|p| p == "-jar"),
        _ => false,
    }
}

/// La riga di shell avvia un server?
///
/// Vera se ALMENO UNO dei comandi della catena lo fa: `cd backend && npm run dev`
/// e `tsc -p . && node dist/server.js` avviano entrambi un server, e giudicare
/// la riga dal primo token li perdeva entrambi (il secondo era per giunta
/// bocciato dal token `" build"` di `is_long_oneshot`).
///
/// La riga viene letta DUE volte, e basta che una delle due riconosca il
/// server. La scomposizione e' POSIX — `\` e' un carattere di escape — mentre
/// su Windows e' il separatore di percorso: `node_modules\.bin\next.cmd dev`
/// si riduce altrimenti a `node_modules.binnext.cmd`, e l'eseguibile diventa
/// irriconoscibile. Sbagliare qui non e' neutro: un servizio non riconosciuto
/// non riceve `PORT` e ripiega sulla porta scritta nel codice, che e' il modo
/// in cui un progetto chiude con zero allocazioni.
///
/// Il rischio speculare — una riga che usa `\` come escape vero e che,
/// normalizzata, sembri un server — non si materializza: a decidere e'
/// l'ESEGUIBILE, cioe' la prima parola del comando, e un escape non ne cambia
/// la natura.
pub(crate) fn riga_avvia_server(riga: &str) -> bool {
    if comandi(riga).iter().any(comando_avvia_server) {
        return true;
    }
    riga.contains('\\') && comandi(&riga.replace('\\', "/")).iter().any(comando_avvia_server)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Attraversa `riga_avvia_server`, cioe' la stessa porta della produzione
    /// (regola O): scompone davvero la riga invece di ricostruirne i token.
    fn avvia(riga: &str) -> bool {
        riga_avvia_server(riga)
    }

    #[test]
    fn un_comando_che_nomina_un_server_non_lo_avvia() {
        // ROOT CAUSE misurata: `contains("vite")` era vero per `VITE_API_URL`,
        // e questo grep ha fermato un vite vivo da 3h45m.
        assert!(!avvia(r#"grep -r "VITE_API_URL" frontend/ --include="*.ts""#));
        assert!(!avvia(r#"node -e "const fs=require('fs'); fs.readFileSync('vite.config.ts')""#));
        assert!(!avvia("cat vite.config.ts"));
        assert!(!avvia("sed -i 's/VITE_PORT=24804/VITE_PORT=24805/' .env"));
        assert!(!avvia("echo $VITE_PORT"));
        assert!(!avvia("ls node_modules/.pnpm 2>&1 | grep -i esbuild"));
        assert!(!avvia(r"cd /d D:\IDEAI-projects\catalogo-libri && dir"));
    }

    #[test]
    fn chiedere_la_versione_non_avvia_niente() {
        // Misurato: ha fermato un servizio vivo da 18 minuti.
        assert!(!avvia("node_modules/.bin/vite --version"));
        assert!(!avvia("npx vite --version"));
        assert!(!avvia("next --help"));
        assert!(!avvia("cargo run --help"));
    }

    #[test]
    fn i_server_veri_restano_riconosciuti() {
        assert!(avvia("vite --port 24804 --host 0.0.0.0 --strictPort"));
        assert!(avvia("npm run dev"));
        assert!(avvia("npm start"));
        assert!(avvia("pnpm dev"));
        assert!(avvia("yarn serve"));
        assert!(avvia("PORT=24828 pnpm --dir ./backend dev"));
        assert!(avvia("pnpm --filter bacheca-attivita-backend dev"));
        assert!(avvia("next dev"));
        assert!(avvia("ng serve"));
        assert!(avvia("gunicorn app:api --bind 0.0.0.0:8000"));
        assert!(avvia("uvicorn main:app --reload"));
        assert!(avvia("cargo run"));
        assert!(avvia("go run ./cmd/server"));
        assert!(avvia("dotnet watch"));
        assert!(avvia("php -S localhost:8000"));
        assert!(avvia("python manage.py runserver"));
        assert!(avvia("python -m uvicorn app:main"));
        assert!(avvia("rails server"));
        assert!(avvia("deno task dev"));
        assert!(avvia("nodemon --watch src src/server.js"));
        // `bun` e `deno` sono insieme lanciatori e runtime: saltarli come
        // lanciatori toglieva di mezzo lo script su cui va posta la domanda.
        assert!(avvia("bun src/index.ts"));
        assert!(avvia("bun run dev"));
        assert!(avvia("deno run api/server.ts"));
        assert!(avvia("node src/backend/server.js"));
        assert!(avvia("tsx api/server.ts"));
    }

    #[test]
    fn il_sottocomando_distingue_servire_da_compilare() {
        // `vite` nudo serve, `vite build` compila: col token nudo erano
        // indistinguibili e solo un gate esterno salvava il secondo.
        assert!(avvia("vite"));
        assert!(avvia("vite dev"));
        assert!(!avvia("vite build"));
        // Il VALORE di un flag non e' un sottocomando: cercare «il primo
        // argomento non-flag» faceva di `24804` il sottocomando di `vite`, e
        // nessun vocabolario lo ammette. Il dev server piu' comune del parco
        // smetteva di essere riconosciuto.
        assert!(avvia("vite --port 24804"));
        assert!(avvia("vite --config vite.config.ts"));
        assert!(!avvia("vite build --outDir dist"));
        assert!(!avvia("npx vite build"));
        assert!(!avvia("cargo build"));
        assert!(!avvia("cargo test"));
        assert!(!avvia("next build"));
        assert!(!avvia("nuxt build"));
    }

    #[test]
    fn compila_e_avvia_e_un_avvio() {
        // Famiglia misurata come integralmente invisibile al criterio vecchio:
        // il token `" build"` la bocciava e il primo token non era un server.
        assert!(avvia("tsc --project tsconfig.json && node backend/dist/server.js"));
        assert!(avvia("pnpm build && pnpm start"));
        assert!(avvia("cd frontend && pnpm build && npx serve -l 24806 dist"));
        assert!(avvia("cd backend && npm run dev"));
    }

    #[test]
    fn lo_scaffolding_non_e_un_servizio() {
        // Il caso che ha aperto l'indagine: 17 righe `kind='service'` con label
        // `backend`, ognuna capace di fermare il backend vero.
        assert!(!avvia("npx prisma init --datasource-provider postgresql"));
        assert!(!avvia("npx prisma init"));
        assert!(!avvia("cd backend && npx prisma init --datasource-provider postgres"));
        assert!(!avvia("npm init -y"));
        assert!(!avvia("npx prisma generate"));
        assert!(!avvia("npx prisma migrate dev"));
        assert!(!avvia("pnpm install"));
        assert!(!avvia("npx eslint ."));
        assert!(!avvia("git status"));
    }

    #[test]
    fn il_percorso_dell_eseguibile_non_cambia_la_risposta() {
        assert!(avvia("./node_modules/.bin/vite --port 3000"));
        assert!(avvia(r"node_modules\.bin\next.cmd dev"));
        assert!(!avvia("./node_modules/.bin/tsc --noEmit"));
        // Un percorso che CONTIENE il nome di un server non e' quel server.
        assert!(!avvia("ls ./node_modules/vite/dist"));
        assert!(!avvia(r"cd /d D:\progetti\vite-app && git log"));
    }
}
