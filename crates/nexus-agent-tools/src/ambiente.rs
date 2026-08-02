//! `ambiente`: PUNTO UNICO (regola L) della domanda «dove sta girando questo
//! agente, e cosa puo' davvero invocarci?».
//!
//! # Il difetto che lo motiva
//!
//! Il 02/08/2026, progetto bacheca-attivita, la figura `verify` (sub-run
//! a5f7419c) ha speso il proprio intero budget — 180s, 16 iterazioni — per
//! scoprire per TENTATIVI una cosa che il sistema sapeva gia':
//!
//! ```text
//! iter 12: `which jq`  -> exit 1, "no jq in (/mingw64/bin:/usr/bin:...)"
//! iter 15:             -> exit 0, "jq is NOT installed"
//! iter 16: `sudo apt-get update` -> "binary nexus-sudo-runner non trovato"
//! ```
//!
//! Il PATH della prima riga dice gia' che siamo in Git Bash su Windows. Nessuno
//! l'aveva detto alla figura: la piattaforma non entrava nel suo contesto, e
//! l'unico modo che le restava per conoscerla era pagarla un'iterazione per
//! ipotesi.
//!
//! Peggio: il system prompt del run principale la spingeva nella direzione
//! sbagliata. Il blocco `<privilegi_sistema>` di `system.nexus_base` dichiara
//! «puoi installare pacchetti con `sudo apt-get install -y`», ed e' scritto per
//! un host Linux. Su Windows non e' un'omissione, e' un'affermazione falsa con
//! l'autorita' del system prompt — un vicolo cieco garantito, non indovinato.
//!
//! # Perche' un FATTO e non un flag
//!
//! Il sistema operativo, la shell che eseguira' davvero i comandi e i gestori di
//! pacchetti installati non sono configurazione: sono cio' che c'e'. Si
//! MISURANO, e la misura arriva all'agente per la stessa strada che i suoi
//! comandi percorreranno — la shell la dichiara [`nexus_tool_kit::sandbox::agent_shell`],
//! lo stesso punto unico che `run_command` usa per lanciarli (regola O). Un
//! `settings.agent.platform = 'windows'` sarebbe una seconda verita' da tenere
//! allineata a mano, e la prima volta che divergesse mentirebbe con l'aria di
//! una configurazione.
//!
//! Resta DATO nel DB (regola G) cio' che e' vocabolario: QUALI gestori sondare.
//! Un gestore nuovo e' una riga in `settings`, non un deploy — ed e' anche la
//! ragione per cui il modulo non chiede «siamo su Windows?» ma «questo nome
//! risponde nel PATH?»: inseguire le piattaforme a codice sarebbe la toppa che
//! la regola H vieta.
//!
//! # L'ignoto e' una variante
//!
//! [`Disponibilita::NonInterrogabile`] esiste perche' «non ho potuto guardare»
//! non degradi ne' a «c'e'» ne' a «non c'e'» (regola Q). Un PATH illeggibile e
//! un `apt-get` assente sono due fatti diversi, e solo il secondo autorizza a
//! dire a un agente di cambiare strada.

use std::path::{Path, PathBuf};
use std::time::Duration;

use nexus_cache::TtlCache;
use once_cell::sync::Lazy;
use sqlx::PgPool;

/// Tag d'apertura del blocco. Vive qui perche' e' la FORMA del fatto: chi lo
/// innesta nel prompt chiede a [`blocco_gia_presente`] invece di conoscerla —
/// due punti che sanno com'e' fatto il blocco sono due punti che possono
/// scriverne versioni diverse.
pub const TAG_BLOCCO: &str = "<ambiente_esecuzione>";

/// Il testo porta gia' una dichiarazione d'ambiente.
pub fn blocco_gia_presente(testo: &str) -> bool {
    testo.contains(TAG_BLOCCO)
}

/// Chiave del vocabolario: nomi dei gestori di pacchetti da sondare (CSV).
pub const K_GESTORI: &str = "agent.environment.package_managers";

/// Chiave del gestore che la direttiva `<privilegi_sistema>` PRESUPPONE. Se quel
/// nome non e' disponibile qui, la direttiva sta affermando una capacita' che
/// l'host non ha e va tolta dal system prompt invece di essere creduta.
pub const K_GESTORE_PRIVILEGIATO: &str = "agent.environment.privileged_install_manager";

/// Per quanto un rilevamento resta valido. Il sistema operativo e la shell non
/// cambiano mai sotto il processo; un gestore installato a mano nel frattempo si
/// vede al giro dopo. Il valore non e' una soglia da tarare (niente setting): e'
/// solo il tempo che si accetta di NON accorgersi di un'installazione nuova.
const TTL_RILEVAMENTO: Duration = Duration::from_secs(300);

/// Cache del rilevamento, chiavata sul vocabolario: cambiare la riga in
/// `settings` cambia la chiave, quindi il giro successivo risonda senza attendere
/// la scadenza ne' un riavvio.
static CACHE: Lazy<TtlCache<String, AmbienteEsecuzione>> =
    Lazy::new(|| TtlCache::new(TTL_RILEVAMENTO));

/// Esito dell'interrogazione su un nome eseguibile.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Disponibilita {
    /// Il nome risponde nel PATH di questo host.
    Disponibile,
    /// Il PATH e' stato letto e quel nome non c'e'.
    Assente,
    /// Il PATH non e' leggibile: non si e' potuto guardare. NON e' «assente».
    NonInterrogabile,
}

impl Disponibilita {
    /// Come si legge nel blocco di prompt. Vocabolario per l'umano e per il
    /// modello; le decisioni si prendono sulla variante.
    fn etichetta(self) -> &'static str {
        match self {
            Disponibilita::Disponibile => "disponibile",
            Disponibilita::Assente => "NON disponibile",
            Disponibilita::NonInterrogabile => "non verificabile",
        }
    }
}

/// Un gestore di pacchetti sondato, col suo esito.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GestorePacchetti {
    pub nome: String,
    pub stato: Disponibilita,
}

/// Cio' che l'host offre davvero all'agente.
///
/// Il testo per il prompt si compone DA questi campi ([`Self::blocco`]), mai il
/// contrario: nessun consumatore rilegge la prosa per sapere se `apt-get` c'e'
/// (regola Q).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AmbienteEsecuzione {
    /// `std::env::consts::OS` del processo che esegue i comandi dell'agente.
    pub sistema_operativo: &'static str,
    /// La shell con cui i comandi verranno REALMENTE eseguiti, dal punto unico
    /// che li esegue.
    pub shell: String,
    /// I gestori del vocabolario, ciascuno col proprio esito. Vuoto = vocabolario
    /// non configurato: il blocco lo DICE, non conclude «nessun gestore».
    pub gestori: Vec<GestorePacchetti>,
}

impl AmbienteEsecuzione {
    /// Esito per un nome del vocabolario. Un nome mai sondato e'
    /// [`Disponibilita::NonInterrogabile`]: non e' stato guardato, e quindi non
    /// si puo' dire che non ci sia.
    pub fn stato_di(&self, nome: &str) -> Disponibilita {
        self.gestori
            .iter()
            .find(|g| g.nome.eq_ignore_ascii_case(nome.trim()))
            .map(|g| g.stato)
            .unwrap_or(Disponibilita::NonInterrogabile)
    }

    /// Blocco di prompt che DICHIARA l'ambiente.
    ///
    /// I gestori assenti sono nominati uno per uno: e' l'unica parte che
    /// impedisce il giro di tentativi, perche' «non e' scritto che c'e'» non
    /// vale come «non c'e'» per un modello addestrato su host Linux.
    pub fn blocco(&self) -> String {
        let mut b = format!("{TAG_BLOCCO}\n");
        b.push_str(&format!(
            "Sistema operativo dell'host: {}.\n",
            self.sistema_operativo
        ));
        b.push_str(&format!(
            "Shell con cui i tuoi comandi vengono eseguiti: {}.\n",
            self.shell
        ));
        if self.gestori.is_empty() {
            b.push_str(
                "Gestori di pacchetti: elenco da sondare non configurato \
                 (settings 'agent.environment.package_managers'), quindi non e' \
                 stato verificato quali esistano. Accertane la presenza prima di \
                 usarne uno.\n",
            );
        } else {
            b.push_str("Gestori di pacchetti su questo host:\n");
            for g in &self.gestori {
                b.push_str(&format!("- {}: {}\n", g.nome, g.stato.etichetta()));
            }
            if self
                .gestori
                .iter()
                .any(|g| g.stato == Disponibilita::Assente)
            {
                b.push_str(
                    "Non invocare i gestori marcati NON disponibile: non esistono qui \
                     e ogni tentativo consuma il tuo budget senza cambiare nulla. Se \
                     manca uno strumento e nessun gestore disponibile puo' \
                     installarlo, dichiaralo come esito bloccato invece di ritentare.\n",
                );
            }
        }
        b.push_str("</ambiente_esecuzione>");
        b
    }
}

/// Rileva l'ambiente, con cache.
///
/// Non fallisce mai verso l'alto: un DB irraggiungibile lascia il vocabolario
/// vuoto, e un ambiente che dichiara solo sistema operativo e shell resta piu'
/// informativo del silenzio.
pub async fn rileva(db: &PgPool) -> AmbienteEsecuzione {
    let vocabolario = carica_vocabolario(db).await;
    let chiave = vocabolario.join(",");
    if let Some(hit) = CACHE.get(&chiave) {
        return hit;
    }
    let ambiente = rileva_con_vocabolario(&vocabolario);
    CACHE.insert(chiave, ambiente.clone());
    ambiente
}

/// Parte MISURABILE senza DB: dato il vocabolario, guarda l'host.
///
/// Separata da [`rileva`] per la stessa ragione per cui `ui_styling` separa il
/// criterio dalla raccolta: un test puo' attraversarla col vocabolario che
/// vuole, senza un Postgres e senza fabbricare l'esito che vuole verificare.
pub fn rileva_con_vocabolario(vocabolario: &[String]) -> AmbienteEsecuzione {
    AmbienteEsecuzione {
        sistema_operativo: std::env::consts::OS,
        // La shell la dichiara chi la lancia: se un domani cambia (override
        // `NEXUS_SHELL`, altro percorso d'installazione di Git Bash), il prompt
        // cambia con lei senza che nessuno se ne ricordi.
        shell: nexus_tool_kit::sandbox::agent_shell(),
        gestori: vocabolario
            .iter()
            .map(|nome| GestorePacchetti {
                nome: nome.clone(),
                stato: nel_path(nome),
            })
            .collect(),
    }
}

/// Il nome risponde nel PATH di questo host?
///
/// In-process, senza spawnare `which`/`where`: la domanda si pone a ogni
/// composizione di prompt, e un processo per candidato la renderebbe un costo
/// invece di un fatto. Su Windows la ricerca prova anche le estensioni
/// eseguibili di `PATHEXT`, che e' il motivo per cui `npm` (un `npm.cmd`) non
/// risulterebbe mai altrimenti.
pub fn nel_path(nome: &str) -> Disponibilita {
    let nome = nome.trim();
    if nome.is_empty() {
        return Disponibilita::NonInterrogabile;
    }
    // Un nome gia' assoluto non si cerca: si guarda.
    let diretto = Path::new(nome);
    if diretto.is_absolute() {
        return if diretto.is_file() {
            Disponibilita::Disponibile
        } else {
            Disponibilita::Assente
        };
    }
    let Some(path) = std::env::var_os("PATH") else {
        // Senza PATH non si e' potuto guardare: dirlo assente sarebbe inventare
        // un fatto su cui un agente cambierebbe strada.
        return Disponibilita::NonInterrogabile;
    };
    let estensioni = estensioni_eseguibili();
    for dir in std::env::split_paths(&path) {
        if candidati(&dir, nome, &estensioni).any(|c| c.is_file()) {
            return Disponibilita::Disponibile;
        }
    }
    Disponibilita::Assente
}

/// I percorsi da provare per un nome dentro una directory del PATH.
fn candidati<'a>(
    dir: &'a Path,
    nome: &'a str,
    estensioni: &'a [String],
) -> impl Iterator<Item = PathBuf> + 'a {
    std::iter::once(dir.join(nome)).chain(
        estensioni
            .iter()
            .map(move |ext| dir.join(format!("{nome}{ext}"))),
    )
}

/// Estensioni che rendono eseguibile un nome su questo host. Vuoto fuori da
/// Windows, dove il nome nudo e' gia' l'eseguibile.
fn estensioni_eseguibili() -> Vec<String> {
    if !cfg!(windows) {
        return Vec::new();
    }
    std::env::var_os("PATHEXT")
        .map(|v| {
            std::env::split_paths(&v)
                .filter_map(|p| p.as_os_str().to_str().map(str::to_ascii_lowercase))
                .filter(|e| !e.is_empty())
                .collect()
        })
        .unwrap_or_else(|| {
            // PATHEXT assente su Windows: le tre estensioni che il sistema
            // considera eseguibili anche senza la variabile.
            [".exe", ".cmd", ".bat"]
                .iter()
                .map(|s| s.to_string())
                .collect()
        })
}

/// Nome del gestore che la direttiva `<privilegi_sistema>` presuppone, dal DB.
/// `None` = non dichiarato: la direttiva non viene toccata (nessuna rimozione
/// decisa su un presupposto che non e' scritto da nessuna parte).
pub async fn gestore_privilegiato(db: &PgPool) -> Option<String> {
    leggi(db, K_GESTORE_PRIVILEGIATO).await
}

/// Vocabolario dei gestori da sondare. Assente -> vuoto: il blocco lo dichiara.
async fn carica_vocabolario(db: &PgPool) -> Vec<String> {
    leggi(db, K_GESTORI)
        .await
        .map(|v| {
            v.split(',')
                .map(str::trim)
                .filter(|s| !s.is_empty())
                .map(str::to_string)
                .collect()
        })
        .unwrap_or_default()
}

async fn leggi(db: &PgPool, key: &str) -> Option<String> {
    nexus_auth::get_setting_checked(db, key)
        .await
        .ok()
        .flatten()
        .map(|v| v.trim().to_string())
        .filter(|v| !v.is_empty())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Un nome che su QUALUNQUE host di sviluppo esiste (il compilatore che sta
    /// eseguendo questo test) e uno che non puo' esistere. Il fatto arriva dal
    /// PATH reale del processo, non da una tabella scritta nel test (regola O).
    #[test]
    fn il_sondaggio_distingue_presente_e_assente_sul_path_reale() {
        assert_eq!(nel_path("cargo"), Disponibilita::Disponibile);
        assert_eq!(
            nel_path("gestore-che-non-esiste-xyz-123"),
            Disponibilita::Assente
        );
    }

    /// Il nome vuoto non e' «assente»: non e' una domanda ponibile.
    #[test]
    fn un_nome_vuoto_non_e_assente() {
        assert_eq!(nel_path("   "), Disponibilita::NonInterrogabile);
    }

    /// IL caso misurato. Il blocco deve NOMINARE il gestore che non c'e': e'
    /// quella riga a chiudere il giro di tentativi, e un blocco che elencasse i
    /// soli disponibili lascerebbe l'assenza da dedurre — cioe' da indovinare.
    ///
    /// MUTAZIONE: filtrare i gestori assenti nel rendering di [`AmbienteEsecuzione::blocco`]
    /// (tenere solo i `Disponibile`, che e' la forma "pulita" a cui verrebbe
    /// naturale ridurlo) fa cadere entrambe le asserzioni sull'assenza.
    #[test]
    fn il_blocco_nomina_i_gestori_assenti() {
        let ambiente = AmbienteEsecuzione {
            sistema_operativo: "windows",
            shell: r"C:\Program Files\Git\bin\bash.exe".to_string(),
            gestori: vec![
                GestorePacchetti {
                    nome: "apt-get".to_string(),
                    stato: Disponibilita::Assente,
                },
                GestorePacchetti {
                    nome: "winget".to_string(),
                    stato: Disponibilita::Disponibile,
                },
            ],
        };
        let b = ambiente.blocco();
        assert!(b.contains("apt-get: NON disponibile"), "{b}");
        assert!(b.contains("winget: disponibile"), "{b}");
        assert!(b.contains("windows"), "{b}");
        assert!(b.contains("bash.exe"), "{b}");
        // L'istruzione a non ritentare compare solo quando c'e' davvero
        // qualcosa da non ritentare.
        assert!(b.contains("Non invocare i gestori marcati NON disponibile"), "{b}");
    }

    /// Vocabolario assente: il blocco dichiara di non aver guardato. Non dice
    /// «nessun gestore disponibile», che sarebbe il falso positivo peggiore —
    /// avrebbe l'aria di una rilevazione.
    #[test]
    fn senza_vocabolario_il_blocco_dichiara_di_non_aver_guardato() {
        let ambiente = rileva_con_vocabolario(&[]);
        let b = ambiente.blocco();
        assert!(b.contains("non configurato"), "{b}");
        assert!(!b.contains("NON disponibile"), "{b}");
        // Sistema operativo e shell si sanno comunque: sono l'ossatura del fatto.
        assert!(b.contains(std::env::consts::OS), "{b}");
    }

    /// Un nome mai sondato non e' assente. La distinzione conta perche' un
    /// consumatore (il gate della direttiva privilegiata) toglie testo dal
    /// system prompt solo su un'assenza ACCERTATA.
    #[test]
    fn un_gestore_fuori_vocabolario_non_e_assente() {
        let ambiente = rileva_con_vocabolario(&["cargo".to_string()]);
        assert_eq!(ambiente.stato_di("cargo"), Disponibilita::Disponibile);
        assert_eq!(
            ambiente.stato_di("apt-get"),
            Disponibilita::NonInterrogabile,
            "mai sondato != assente"
        );
    }

    /// La shell dichiarata e' quella che esegue davvero: il test la confronta col
    /// produttore, non con una stringa attesa (regola O). Se un domani
    /// `agent_shell` cambia, il blocco cambia con lei e questo test resta vero —
    /// mentre un letterale `"bash"` misurerebbe solo se stesso.
    #[test]
    fn la_shell_dichiarata_e_quella_che_esegue() {
        let ambiente = rileva_con_vocabolario(&[]);
        assert_eq!(ambiente.shell, nexus_tool_kit::sandbox::agent_shell());
    }

    /// Su Windows un `.cmd`/`.exe` non si trova cercando il nome nudo: senza le
    /// estensioni il sondaggio direbbe «assente» per strumenti presenti, e il
    /// blocco vieterebbe all'agente cio' che invece puo' usare.
    #[cfg(windows)]
    #[test]
    fn su_windows_le_estensioni_eseguibili_entrano_nella_ricerca() {
        let ext = estensioni_eseguibili();
        assert!(!ext.is_empty());
        assert!(ext.iter().any(|e| e == ".exe"), "{ext:?}");
        // `where.exe` esiste su ogni Windows: si trova solo con l'estensione.
        assert_eq!(nel_path("where"), Disponibilita::Disponibile);
    }
}
