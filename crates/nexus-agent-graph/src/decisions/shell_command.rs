//! Punto unico (regola L) della SCOMPOSIZIONE di una riga di shell nei suoi
//! comandi semplici: separatori (`&&`, `||`, `;`, `|`, `&`, newline)
//! riconosciuti FUORI dalle virgolette, redirezioni tolte dalle parole e
//! segnalate a parte, assegnazioni env inline separate dall'eseguibile,
//! escape `\` risolti.
//!
//! Non e' una shell: non espande variabili, glob o sostituzioni di comando.
//! Serve a RICONOSCERE cosa la riga chiede, mai a deciderne l'esecuzione al
//! posto della shell vera. Chi legge poi un [`Comando`] decide la propria
//! domanda sui suoi campi:
//!
//! - `mcp-core::agent_tools::playwright_cli` chiede «questa riga chiede la
//!   SUITE Playwright?» (il RICONOSCIMENTO resta li', delega la scomposizione);
//! - `decisions::step_gate` chiede «i token del comando contengono il pattern
//!   `rm -rf`?» (matcher a sottosequenza contigua sulle `parole`).
//!
//! Perche' qui e non in mcp-core: due scompositori indipendenti divergevano
//! nel SILENZIO. Misurato: `2>&1` produceva nell'ex scompositore di step_gate
//! un comando fantasma `["1"]` e un token spurio `"2>"` (l'`&` di `2>&1`
//! trattato come separatore), mentre questo scompositore isola la redirezione
//! e non lascia token spuri; l'escape `\` era gestito solo qui; e `FOO=1` era
//! lasciato fra i token la', mentre qui va in `env`. Un pattern
//! `command_token` sul primo poteva vedere token che non erano parole del
//! comando. La direzione della delega e' obbligata dal grafo delle
//! dipendenze: mcp-core dipende da nexus-agent-graph, mai il contrario.

/// Un comando semplice della catena shell, gia' scomposto.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Comando {
    /// Assegnazioni `NOME=valore` che precedono l'eseguibile.
    pub env: Vec<(String, String)>,
    /// Parole del comando, virgolette risolte, redirezioni escluse.
    pub parole: Vec<String>,
    /// La riga portava redirezioni su questo comando (`>`, `2>&1`, ...).
    pub redirezioni: bool,
}

/// Scompone una riga di shell nei suoi comandi semplici (punto unico).
pub fn comandi(riga: &str) -> Vec<Comando> {
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

#[cfg(test)]
mod tests {
    use super::*;

    fn parole_di(riga: &str) -> Vec<Vec<String>> {
        comandi(riga).into_iter().map(|c| c.parole).collect()
    }

    #[test]
    fn separatori_fuori_dalle_virgolette() {
        assert_eq!(
            parole_di("rm -rf a && echo b"),
            vec![vec!["rm", "-rf", "a"], vec!["echo", "b"]],
        );
        // La stringa quotata NON e' spezzata dai separatori interni.
        assert_eq!(
            parole_di("echo 'a && b'"),
            vec![vec!["echo", "a && b"]],
        );
    }

    /// Il caso misurato: `2>&1` NON deve produrre un comando fantasma ne'
    /// token spuri. Mutazione: togliere il ramo redirezione dal match di
    /// `comandi` -> l'`&` spezza e rinasce il fantasma `["1"]` -> rosso.
    #[test]
    fn redirezione_non_lascia_token_spuri() {
        let c = comandi("node app.js 2>&1");
        assert_eq!(c.len(), 1, "un solo comando, nessun fantasma");
        assert_eq!(c[0].parole, vec!["node", "app.js"]);
        assert!(c[0].redirezioni);
        // Il bersaglio staccato viene scartato, non diventa una parola.
        assert_eq!(parole_di("cat x > out.log"), vec![vec!["cat", "x"]]);
    }

    #[test]
    fn env_inline_separato_dalle_parole() {
        let c = comandi("FOO=1 BAR=2 rm -rf x");
        assert_eq!(c.len(), 1);
        assert_eq!(c[0].env, vec![("FOO".into(), "1".into()), ("BAR".into(), "2".into())]);
        assert_eq!(c[0].parole, vec!["rm", "-rf", "x"]);
        // `--flag=x` NON e' env: resta parola.
        assert_eq!(parole_di("cmd --flag=x"), vec![vec!["cmd", "--flag=x"]]);
    }

    #[test]
    fn escape_backslash_gestito() {
        // `\ ` (spazio escapato) resta nella stessa parola.
        assert_eq!(parole_di(r"rm foo\ bar"), vec![vec!["rm", "foo bar"]]);
    }
}
