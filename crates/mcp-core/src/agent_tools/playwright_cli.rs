//! Punto unico (regola L): «questa riga di shell chiede la SUITE Playwright?»,
//! e in caso affermativo chi la esegue.
//!
//! La suite ha UN esecutore, `testing::tool_run_playwright_tests`: e' li' che
//! vivono la BASE_URL derivata dalle porte allocate, il preflight di Chromium,
//! lo streaming live e il record `jobs` del pannello, ed e' li' che si innesta
//! l'attesa che il servizio bersaglio risponda prima del lancio. Un secondo
//! percorso che lanci la stessa suite senza passare di li' non e' una
//! scorciatoia: e' un esecutore diverso con un contratto diverso, e il rosso
//! che produce e' indistinguibile da quello vero. Vale in particolare per una
//! garanzia di readiness: messa su uno dei due percorsi, e' il percorso che
//! l'agente sceglie a decidere se vale.
//!
//! Era il caso di `run_command` / `run_tests`: qualunque riga contenente
//! "playwright" veniva eseguita in proprio e il job registrato A POSTERIORI
//! (`record_playwright_job`, rimosso con questo modulo) col medesimo
//! `kind='playwright_test'`. Due difetti in uno: la suite partiva senza alcuna
//! delle garanzie del runner, e nel pannello il suo esito era indistinguibile
//! da quello prodotto dall'esecutore vero. Il riconoscimento era per giunta
//! `command.contains("playwright")`, quindi `npx playwright install`,
//! `npx playwright show-report` e perfino `cat playwright.config.ts`
//! registravano un "test" mai eseguito.
//!
//! Qui il riconoscimento e' LESSICALE, non testuale: la riga viene scomposta
//! in comandi e parole (rispettando le virgolette), e la domanda diventa «una
//! di queste parole e' l'eseguibile playwright, e il suo sottocomando e'
//! `test`?». Non e' la regola M al contrario: la regola M vieta di dedurre lo
//! STATO di una richiesta dal testo umano di un messaggio, mentre qui il testo
//! E' l'oggetto — una command line e' un dato sintattico, e l'unico modo di
//! leggerla e' scomporla come farebbe la shell che poi la esegue.
//!
//! Rapporto con `privileged.rs`: quel modulo scompone la stessa riga per una
//! domanda diversa («richiede privilegi di sistema?») e con un confine di
//! sicurezza deliberatamente piu' stretto (`has_shell_metachars` rifiuta di
//! instradare qualunque riga composita, perche' a valle ESEGUE un'azione
//! privilegiata). Le due scomposizioni restano distinte: la sua produce una
//! STRINGA normalizzata su cui poi si validano nomi di pacchetto, e farle
//! convergere cambierebbe il testo su cui decide un percorso privilegiato —
//! un consolidamento che va misurato per se', non appeso a questo.

use super::AgentToolContext;

/// Un comando semplice della catena shell, gia' scomposto.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct Comando {
    /// Assegnazioni `NOME=valore` che precedono l'eseguibile.
    pub env: Vec<(String, String)>,
    /// Parole del comando, virgolette risolte, redirezioni escluse.
    pub parole: Vec<String>,
    /// La riga portava redirezioni su questo comando (`>`, `2>&1`, ...).
    pub redirezioni: bool,
}

/// Invocazione della suite riconosciuta in una riga.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct InvocazioneSuite {
    /// Directory dichiarata dai `cd` che precedono l'invocazione.
    pub cd: Option<String>,
    /// Argomenti dopo `test`, nell'ordine originale.
    pub args: Vec<String>,
    /// Assegnazioni env inline del comando (`BASE_URL=... npx playwright test`).
    pub env_inline: Vec<(String, String)>,
    /// La riga portava redirezioni sul comando della suite.
    pub redirezioni: bool,
    /// La riga porta altri comandi oltre alla suite e ai `cd` che la precedono.
    /// La delega non e' possibile: eseguirebbe solo la suite, saltando in
    /// silenzio il resto della catena.
    pub composita: bool,
}

/// Scompone una riga di shell nei suoi comandi semplici.
///
/// Punto unico (regola L) della scomposizione: separatori (`&&`, `||`, `;`,
/// `|`, `&`, newline) riconosciuti FUORI dalle virgolette, redirezioni tolte
/// dalle parole e segnalate a parte, assegnazioni env inline separate
/// dall'eseguibile. Non e' una shell: non espande variabili, glob o
/// sostituzioni di comando — serve a RICONOSCERE cosa la riga chiede, mai a
/// deciderne l'esecuzione al posto della shell vera.
pub(super) fn comandi(riga: &str) -> Vec<Comando> {
    let mut s = Scomposizione::default();
    let chars: Vec<char> = riga.chars().collect();
    let mut i = 0;

    while i < chars.len() {
        let c = chars[i];
        i = match c {
            '\'' | '"' => s.consuma_stringa(&chars, i),
            '\\' if i + 1 < chars.len() => {
                s.corrente.push(chars[i + 1]);
                s.ha_corrente = true;
                i + 2
            }
            c if c.is_whitespace() && c != '\n' => {
                s.chiudi_parola();
                i + 1
            }
            '\n' | ';' => {
                s.chiudi_comando();
                i + 1
            }
            '&' | '|' => {
                s.chiudi_comando();
                // `&&` e `||` sono un separatore solo, come `&` e `|` singoli.
                i + if i + 1 < chars.len() && chars[i + 1] == c {
                    2
                } else {
                    1
                }
            }
            '>' | '<' => s.consuma_redirezione(&chars, i),
            _ => {
                s.corrente.push(c);
                s.ha_corrente = true;
                i + 1
            }
        };
    }
    s.chiudi_comando();
    s.comandi
}

/// Stato della scansione di [`comandi`].
#[derive(Default)]
struct Scomposizione {
    comandi: Vec<Comando>,
    parole: Vec<String>,
    corrente: String,
    ha_corrente: bool,
    redirezioni: bool,
    /// La prossima parola e' il bersaglio di una redirezione, non un argomento.
    attesa_target: bool,
}

impl Scomposizione {
    /// Consuma una stringa quotata a partire dall'apice in `i`, aggiungendone
    /// il contenuto alla parola in costruzione. Ritorna l'indice successivo
    /// alla chiusura (o la fine della riga, se l'apice non e' chiuso).
    fn consuma_stringa(&mut self, chars: &[char], i: usize) -> usize {
        let apice = chars[i];
        let mut j = i + 1;
        self.ha_corrente = true;
        while j < chars.len() && chars[j] != apice {
            self.corrente.push(chars[j]);
            j += 1;
        }
        j + 1
    }

    /// Consuma una redirezione (`>`, `>>`, `2>`, `2>&1`, `<`) e i suoi spazi:
    /// segnala il fatto e arma lo scarto del bersaglio, che non e' una parola
    /// del comando. Ritorna l'indice della prossima cosa da leggere.
    fn consuma_redirezione(&mut self, chars: &[char], i: usize) -> usize {
        // La parola in costruzione, se e' il solo numero di descrittore
        // (`2>`), non e' una parola del comando.
        if self.ha_corrente && self.corrente.chars().all(|c| c.is_ascii_digit()) {
            self.corrente.clear();
            self.ha_corrente = false;
        } else {
            self.chiudi_parola();
        }
        self.redirezioni = true;
        let mut j = i + 1;
        // Operatore esteso: `>>`, `>&1`, `&>`.
        while j < chars.len() && matches!(chars[j], '>' | '&') {
            j += 1;
        }
        // Il bersaglio, attaccato (`>/dev/null`) o staccato (`> out.log`), non
        // e' una parola del comando.
        self.attesa_target = true;
        while j < chars.len() && chars[j].is_whitespace() && chars[j] != '\n' {
            j += 1;
        }
        j
    }

    fn chiudi_parola(&mut self) {
        if !self.ha_corrente {
            return;
        }
        let parola = std::mem::take(&mut self.corrente);
        self.ha_corrente = false;
        if self.attesa_target {
            self.attesa_target = false;
        } else {
            self.parole.push(parola);
        }
    }

    fn chiudi_comando(&mut self) {
        self.chiudi_parola();
        self.attesa_target = false;
        let parole = std::mem::take(&mut self.parole);
        let redirezioni = std::mem::take(&mut self.redirezioni);
        if !parole.is_empty() {
            self.comandi.push(Comando::nuovo(parole, redirezioni));
        }
    }
}

impl Comando {
    /// Separa le assegnazioni env inline (`NOME=valore` in testa) dalle parole
    /// del comando vero e proprio.
    fn nuovo(parole: Vec<String>, redirezioni: bool) -> Self {
        let mut env = Vec::new();
        let mut resto = Vec::new();
        let mut ancora_env = true;
        for p in parole {
            if ancora_env {
                if let Some((nome, valore)) = assegnazione_env(&p) {
                    env.push((nome, valore));
                    continue;
                }
                ancora_env = false;
            }
            resto.push(p);
        }
        Comando {
            env,
            parole: resto,
            redirezioni,
        }
    }
}

/// `NOME=valore` con NOME identificatore di shell valido, altrimenti None
/// (`--flag=x` e `foo=bar/baz` come argomento posizionale non lo sono).
fn assegnazione_env(p: &str) -> Option<(String, String)> {
    let (nome, valore) = p.split_once('=')?;
    if nome.is_empty() {
        return None;
    }
    let primo_ok = nome
        .chars()
        .next()
        .is_some_and(|c| c.is_ascii_alphabetic() || c == '_');
    if !primo_ok || !nome.chars().all(|c| c.is_ascii_alphanumeric() || c == '_') {
        return None;
    }
    Some((nome.to_string(), valore.to_string()))
}

/// Lanciatori di pacchetto che precedono l'eseguibile vero.
const LANCIATORI: &[&str] = &["npx", "pnpm", "pnpm.cmd", "npm", "yarn", "bunx", "bun", "deno"];

/// Sottocomandi dei lanciatori che precedono ancora l'eseguibile.
const SOTTOCOMANDI_LANCIATORE: &[&str] = &["exec", "dlx", "run", "x"];

/// Basename dell'eseguibile, senza directory, estensione Windows e `@versione`.
fn eseguibile(parola: &str) -> String {
    let base = parola
        .rsplit(['/', '\\'])
        .next()
        .unwrap_or(parola)
        .to_ascii_lowercase();
    let base = base.split('@').next().unwrap_or(&base).to_string();
    for ext in [".cmd", ".exe", ".ps1", ".bat"] {
        if let Some(s) = base.strip_suffix(ext) {
            return s.to_string();
        }
    }
    base
}

/// Se il comando invoca la suite (`playwright test`), ritorna gli argomenti
/// dopo `test`. None per ogni altro sottocomando (`install`, `show-report`,
/// `codegen`, `--version`) e per le righe che nominano "playwright" senza
/// invocarlo (`cat playwright.config.ts`).
fn args_suite(cmd: &Comando) -> Option<Vec<String>> {
    let mut i = 0;
    // Salta i lanciatori, i loro sottocomandi e i loro flag.
    while i < cmd.parole.len() {
        let p = cmd.parole[i].to_ascii_lowercase();
        if LANCIATORI.contains(&eseguibile(&p).as_str()) {
            i += 1;
            while i < cmd.parole.len() {
                let q = cmd.parole[i].to_ascii_lowercase();
                if q.starts_with('-') || SOTTOCOMANDI_LANCIATORE.contains(&q.as_str()) {
                    i += 1;
                } else {
                    break;
                }
            }
            continue;
        }
        break;
    }
    if eseguibile(cmd.parole.get(i)?) != "playwright" {
        return None;
    }
    // Primo token non-flag dopo l'eseguibile: e' il sottocomando.
    let sottocomando = cmd.parole[i + 1..].iter().position(|p| !p.starts_with('-'))?;
    let idx = i + 1 + sottocomando;
    if cmd.parole[idx] != "test" {
        return None;
    }
    Some(cmd.parole[idx + 1..].to_vec())
}

/// La riga chiede la suite Playwright? Punto unico del riconoscimento.
pub(super) fn invocazione_suite(riga: &str) -> Option<InvocazioneSuite> {
    let cmds = comandi(riga);
    let pos = cmds.iter().position(|c| args_suite(c).is_some())?;
    let suite = &cmds[pos];
    let args = args_suite(suite)?;

    // I `cd` che precedono dichiarano la directory; qualunque ALTRO comando,
    // prima o dopo, rende la riga composita.
    let mut cd = None;
    let mut composita = false;
    for (idx, c) in cmds.iter().enumerate() {
        if idx == pos {
            continue;
        }
        if idx < pos && c.parole.first().map(String::as_str) == Some("cd") && c.parole.len() == 2 {
            cd = Some(c.parole[1].clone());
            continue;
        }
        composita = true;
    }

    Some(InvocazioneSuite {
        cd,
        args,
        env_inline: suite.env.clone(),
        redirezioni: suite.redirezioni,
        composita,
    })
}

/// Guardia unica dei tool generici: se `command` chiede la suite Playwright,
/// la esegue attraverso l'esecutore unico e ritorna il suo output; se la riga
/// e' composita ritorna il rifiuto motivato. `None` = la riga non riguarda la
/// suite, il chiamante prosegue con la propria esecuzione.
///
/// I chiamanti sono i tre tool che ricevono un comando arbitrario dall'agente:
/// `tool_run_command`, `tool_run_tests` e `tool_run_service` (che serve sia
/// `run_service` sia `run_in_terminal`). Chiamano QUESTA, non il
/// riconoscimento, cosi' la decisione e la sua conseguenza restano insieme.
pub(super) async fn intercetta_suite(
    ctx: &AgentToolContext,
    tool: &str,
    command: &str,
    working_dir_param: Option<&str>,
) -> Option<String> {
    let inv = invocazione_suite(command)?;
    if inv.composita {
        return Some(rifiuto_riga_composita(tool, command));
    }
    // Il boxing spezza un ciclo di TIPI, non un rischio a runtime: da qui si va
    // al runner, e il runner con `auto_start_server` puo' avviare il dev server
    // chiamando `tool_run_service`, che a sua volta interroga questa guardia.
    // Il compilatore vede l'anello e chiede una dimensione finita; il ciclo
    // pero' non puo' ripetersi, perche' la delega NON concede l'auto-start (lo
    // lascia al suo default `false`): un runner nato qui non chiama mai
    // `tool_run_service`. Profondita' massima uno, per costruzione.
    Some(
        Box::pin(super::testing::esegui_suite_delegata(
            ctx,
            tool,
            &inv,
            working_dir_param,
            command,
        ))
        .await,
    )
}

/// Lunghezza massima della riga originale citata nel messaggio di rifiuto: un
/// estratto, non l'intera riga (che puo' essere lunga quanto vuole l'agente).
const MAX_CHARS_RIGA_NEL_RIFIUTO: usize = 200;

/// Messaggio per la riga che chiede la suite insieme ad altri comandi.
///
/// Non si delega: il runner eseguirebbe la sola suite e gli altri comandi
/// sparirebbero in silenzio (un `npm ci &&` saltato produce un rosso che
/// nessuno sa spiegare). Non si esegue in proprio: sarebbe il difetto che
/// questo modulo chiude. Si chiede di spezzare, dicendo come.
fn rifiuto_riga_composita(tool: &str, command: &str) -> String {
    format!(
        "\u{274C} [{tool}] Questa riga lancia la suite Playwright insieme ad altri comandi, \
         e la suite ha un solo esecutore: il tool `run_playwright_tests` (porta BASE_URL dalle \
         porte allocate, preflight Chromium, attesa che il servizio bersaglio sia pronto e \
         registrazione nel pannello Playwright).\n\
         Eseguirla da qui la lancerebbe senza nessuna di quelle garanzie; delegarla eseguirebbe \
         solo la suite, saltando in silenzio il resto della catena.\n\
         Spezza in due passi: prima il resto della catena con questo tool, poi la suite con \
         `run_playwright_tests` (parametri: filter, project, workers, reporter, config_path, \
         base_url, test_timeout_ms).\n\
         Riga ricevuta: {}",
        command
            .chars()
            .take(MAX_CHARS_RIGA_NEL_RIFIUTO)
            .collect::<String>()
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parole(riga: &str) -> Vec<Vec<String>> {
        comandi(riga).into_iter().map(|c| c.parole).collect()
    }

    #[test]
    fn scompone_separatori_fuori_dalle_virgolette() {
        assert_eq!(
            parole("npm ci && npx playwright test"),
            vec![
                vec!["npm".to_string(), "ci".to_string()],
                vec![
                    "npx".to_string(),
                    "playwright".to_string(),
                    "test".to_string()
                ],
            ]
        );
        // Il `;` dentro le virgolette non separa: e' testo dell'argomento.
        assert_eq!(
            parole("echo \"a; b\""),
            vec![vec!["echo".to_string(), "a; b".to_string()]]
        );
    }

    #[test]
    fn toglie_le_redirezioni_dalle_parole() {
        let cmds = comandi("npx playwright test 2>&1 > out.log");
        assert_eq!(cmds.len(), 1);
        assert_eq!(
            cmds[0].parole,
            vec![
                "npx".to_string(),
                "playwright".to_string(),
                "test".to_string()
            ]
        );
        assert!(cmds[0].redirezioni, "la redirezione va segnalata a parte");
    }

    #[test]
    fn separa_le_assegnazioni_env_inline() {
        let cmds = comandi("BASE_URL=http://127.0.0.1:5173 npx playwright test");
        assert_eq!(
            cmds[0].env,
            vec![(
                "BASE_URL".to_string(),
                "http://127.0.0.1:5173".to_string()
            )]
        );
        assert_eq!(cmds[0].parole[0], "npx");
    }

    /// Le forme con cui un agente lancia davvero la suite.
    #[test]
    fn riconosce_le_forme_di_invocazione_della_suite() {
        for riga in [
            "npx playwright test",
            "npx --yes playwright test",
            "pnpm exec playwright test",
            "pnpm playwright test",
            "yarn playwright test",
            "bunx playwright test",
            "./node_modules/.bin/playwright test",
            "node_modules/.bin/playwright.cmd test",
            "playwright test",
            "npx playwright@latest test",
        ] {
            assert!(
                invocazione_suite(riga).is_some(),
                "non riconosciuta come suite: {riga}"
            );
        }
    }

    /// Cio' che il riconoscimento testuale `contains("playwright")` registrava
    /// come "playwright_test" pur non essendo un'esecuzione di test.
    #[test]
    fn non_confonde_gli_altri_usi_del_cli() {
        for riga in [
            "npx playwright install --with-deps chromium",
            "npx playwright install-deps",
            "npx playwright show-report",
            "npx playwright codegen http://localhost:3000",
            "npx playwright --version",
            "cat playwright.config.ts",
            "rm -rf playwright-report",
            "npm install -D @playwright/test",
            "npm run test:e2e",
            "echo \"npx playwright test\"",
        ] {
            assert!(
                invocazione_suite(riga).is_none(),
                "riconosciuta a torto come suite: {riga}"
            );
        }
    }

    #[test]
    fn il_cd_dichiara_la_directory_e_non_rende_composita() {
        let inv = invocazione_suite("cd app && npx playwright test --project=chromium")
            .expect("suite riconosciuta");
        assert_eq!(inv.cd.as_deref(), Some("app"));
        assert!(!inv.composita);
        assert_eq!(inv.args, vec!["--project=chromium".to_string()]);
    }

    #[test]
    fn un_altro_comando_nella_catena_rende_la_riga_composita() {
        let inv = invocazione_suite("npm ci && npx playwright test").expect("suite riconosciuta");
        assert!(
            inv.composita,
            "npm ci verrebbe saltato in silenzio dalla delega"
        );
        let dopo = invocazione_suite("npx playwright test && echo fatto").expect("suite");
        assert!(dopo.composita, "anche cio' che segue va dichiarato");
    }

    #[test]
    fn conserva_gli_argomenti_della_suite() {
        let inv = invocazione_suite("npx playwright test e2e/auth.spec.ts --grep \"login utente\" --workers 4")
            .expect("suite riconosciuta");
        assert_eq!(
            inv.args,
            vec![
                "e2e/auth.spec.ts".to_string(),
                "--grep".to_string(),
                "login utente".to_string(),
                "--workers".to_string(),
                "4".to_string(),
            ],
            "gli argomenti dell'agente devono arrivare interi all'esecutore"
        );
    }

    #[test]
    fn env_inline_riconosciuta_sul_comando_della_suite() {
        let inv = invocazione_suite("BASE_URL=http://127.0.0.1:4321 npx playwright test")
            .expect("suite riconosciuta");
        assert_eq!(inv.env_inline.len(), 1);
        assert_eq!(inv.env_inline[0].0, "BASE_URL");
    }
}
