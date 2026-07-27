//! La scelta di provider espressa dall'utente in chat: PREFERENZA o PIN.
//!
//! PUNTO UNICO (regola L) di una domanda che il sistema si pone in piu' posti —
//! "questa richiesta e' vincolata a un fornitore, oppure il routing puo'
//! cambiarlo?" — e che finora nessuno si poneva davvero, perche' il vincolo era
//! DEDOTTO: bastava che il dropdown non fosse su "Auto".
//!
//! PERCHE' ESISTE. Nel composer ci sono due controlli distinti: il dropdown del
//! provider e il pulsante "Forza". Il pulsante non e' mai arrivato al backend
//! (serviva al colore del bordo e a mostrare il dropdown dei modelli), quindi il
//! backend vedeva UNA sola informazione — il nome del provider — e doveva
//! indovinare quanto vincolasse. Finche' l'override non aveva effetto reale la
//! deduzione era innocua; da quando il provider scelto viaggia come
//! `GwRequest::pin_provider` (gateway in `strict`, chain di un solo fornitore,
//! nessun fallback cross-provider) la stessa deduzione trasformerebbe OGNI
//! selezione dal dropdown in un vincolo duro — e i due tooltip del pulsante,
//! che promettono l'opposto ("il routing puo' scegliere un provider diverso"),
//! diventerebbero falsi. Lo stesso vizio di prima, col segno invertito.
//!
//! Le due cose sono quindi DISTINTE e distinguibili sul wire
//! (`providerOverride` + `providerOverrideMode`), con identificatori canonici in
//! inglese (regola N), e il pin duro nasce SOLO qui.

/// Quanto vincola la scelta di provider arrivata sul wire.
///
/// Identificatori canonici (regola N): `preferred` | `pinned`, un solo punto di
/// parse, nessun sinonimo. Sono i due significati che il composer gia' mostra
/// nei tooltip; prima non avevano un nome sul wire e quindi non viaggiavano.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ProviderOverrideMode {
    /// Il provider e' un SUGGERIMENTO: entra nel routing come punto di partenza,
    /// ma la richiesta conserva il fallback (il gateway puo' rispondere con un
    /// altro fornitore). E' il default: l'assenza del campo non deve mai
    /// produrre un vincolo che l'utente non ha chiesto.
    #[default]
    Preferred,
    /// Il provider e' un VINCOLO: la richiesta va solo a lui
    /// (`GwRequest::pin_provider`), il gateway non re-instrada e non ripiega su
    /// un altro fornitore. Se fallisce, la chiamata fallisce e la chat lo dice.
    Pinned,
}

/// Identificatore `provider_override_mode` non canonico (solo `preferred|pinned`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct InvalidProviderOverrideMode;

impl ProviderOverrideMode {
    /// L'identificatore del vincolo debole. Scritto UNA volta: parse, resa e
    /// elenco dei valori ammessi lo leggono da qui, cosi' non possono
    /// divergere (una parola accettata dal parser ma mai emessa, o viceversa,
    /// e' un vocabolario doppio che si scopre solo in produzione).
    pub const PREFERRED: &'static str = "preferred";
    /// L'identificatore del vincolo duro.
    pub const PINNED: &'static str = "pinned";
    /// I valori ammessi sul wire, nell'ordine in cui si mostrano all'utente.
    pub const CANONICAL: [&'static str; 2] = [Self::PREFERRED, Self::PINNED];

    /// L'identificatore canonico di questo stato, per il wire e per i log.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Preferred => Self::PREFERRED,
            Self::Pinned => Self::PINNED,
        }
    }

    /// Parsa l'identificatore canonico (ASCII lowercase esatto). Rifiuta
    /// sinonimi e varianti non inglesi (regola N).
    ///
    /// Campo ASSENTE o vuoto -> `Preferred`, e non e' un errore: e' lo stato di
    /// tutte le superfici che il pulsante "Forza" non ce l'hanno (resend,
    /// riattivazione, worker, client vecchi). Il default deve essere il vincolo
    /// PIU' DEBOLE — un default `Pinned` regalerebbe a quelle superfici un
    /// vincolo che nessuno ha chiesto, che e' esattamente il difetto da cui
    /// nasce questo modulo. Un valore SCRITTO MALE resta invece un errore: se il
    /// client voleva dire qualcosa, deve dirlo con la parola giusta.
    pub fn try_parse(value: Option<&str>) -> Result<Self, InvalidProviderOverrideMode> {
        let Some(raw) = value.map(str::trim).filter(|v| !v.is_empty()) else {
            return Ok(Self::Preferred);
        };
        match raw {
            Self::PREFERRED => Ok(Self::Preferred),
            Self::PINNED => Ok(Self::Pinned),
            _ => Err(InvalidProviderOverrideMode),
        }
    }
}

/// La scelta di provider di UNA richiesta di chat.
///
/// Enum e non `(Option<String>, ProviderOverrideMode)`: la coppia rende
/// rappresentabile lo stato senza senso "pinnato su nessun provider", ed e' il
/// tipo di stato che poi qualcuno legge a meta' (il modo senza il provider, o
/// viceversa). Qui le combinazioni valide sono tre e sono tutte nominate.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub enum ProviderChoice {
    /// Dropdown su "Auto": decide il routing.
    #[default]
    Auto,
    /// Provider scelto senza "Forza", oppure ricordato da una fonte persistita:
    /// suggerimento, il routing puo' cambiarlo.
    Preferred(String),
    /// Provider scelto CON "Forza": vincolo duro, nessun ripiego.
    Pinned(String),
}

impl ProviderChoice {
    /// PUNTO UNICO (regola L) della scelta di provider di una richiesta di chat.
    ///
    /// - `request_provider` / `request_mode`: cio' che il client ha mandato
    ///   ADESSO. Il modo vale SOLO per questo provider — e' l'unica strada per
    ///   cui puo' nascere un pin duro.
    /// - `remembered_provider`: il provider che una fonte persistita ricorda
    ///   (preferenza di sessione in `chat_sessions.preferred_provider`, metadata
    ///   del messaggio in un resend). Vale come PREFERENZA e basta.
    ///
    /// IL PIN NON SI EREDITA. La preferenza di sessione e' scritta sul server
    /// dal solo cambio del dropdown e sopravvive al refresh: se ereditasse
    /// anche la forza del vincolo, ogni messaggio successivo — anche inviato da
    /// una superficie che il pulsante "Forza" non lo ha nemmeno — nascerebbe
    /// pinnato, e una sessione ripresa altrove porterebbe un vincolo invisibile
    /// che nessuno ha chiesto in quel momento (se poi quel provider entra in
    /// cooldown, la sessione resta bloccata senza che l'utente sappia perche').
    /// Un vincolo duro e' un ordine: vale per la richiesta in cui lo si da'.
    pub fn resolve(
        request_provider: Option<&str>,
        request_mode: ProviderOverrideMode,
        remembered_provider: Option<&str>,
    ) -> Self {
        match normalize(request_provider) {
            Some(provider) => match request_mode {
                ProviderOverrideMode::Pinned => Self::Pinned(provider),
                ProviderOverrideMode::Preferred => Self::Preferred(provider),
            },
            None => match normalize(remembered_provider) {
                Some(provider) => Self::Preferred(provider),
                None => Self::Auto,
            },
        }
    }

    /// Il provider scelto, quale che sia la forza del vincolo. E' cio' che entra
    /// nel routing come punto di partenza (stima a ledger, provider di
    /// riferimento per il modello): legittimo anche con la sola preferenza.
    pub fn provider(&self) -> Option<&str> {
        match self {
            Self::Auto => None,
            Self::Preferred(p) | Self::Pinned(p) => Some(p.as_str()),
        }
    }

    /// Il provider PINNATO. Valorizzato solo dal vincolo duro: e' l'unico
    /// ingresso legittimo di `GwRequest::pin_provider` per la chat.
    pub fn pinned_provider(&self) -> Option<&str> {
        match self {
            Self::Pinned(p) => Some(p.as_str()),
            Self::Auto | Self::Preferred(_) => None,
        }
    }

    /// Nome canonico dello stato, per log e telemetria (regola M: lo stato si
    /// dichiara, non si deduce da "c'e' un provider valorizzato?").
    pub fn label(&self) -> &'static str {
        match self {
            Self::Auto => "auto",
            Self::Preferred(_) => ProviderOverrideMode::Preferred.as_str(),
            Self::Pinned(_) => ProviderOverrideMode::Pinned.as_str(),
        }
    }
}

/// Nome provider utilizzabile: trim, scarto del vuoto, lowercase. La
/// normalizzazione vive qui e non nei chiamanti — era in `Orchestrator::run`, e
/// ogni altra superficie che leggeva lo stesso campo doveva ricordarsene.
fn normalize(value: Option<&str>) -> Option<String> {
    value
        .map(str::trim)
        .filter(|v| !v.is_empty())
        .map(str::to_lowercase)
}
