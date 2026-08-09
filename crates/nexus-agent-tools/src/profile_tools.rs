//! Tool agente per la gestione dei profili utente.
//!
//! MIGRATI al contratto d'ingresso e a `RispostaTool` (regola Q).
//!
//! RAMI NUDI CHIUSI, e sono i due esiti piu' probabili di questi tool: «profilo
//! gia' esistente» in creazione e «profilo non trovato» in aggiornamento
//! uscivano come stringhe SENZA marker, cioe' come tool RIUSCITI. In entrambi i
//! casi la scrittura non era avvenuta, e l'agente riceveva la conferma che era
//! avvenuta: la regola M gli vieta di rileggere quel testo per accorgersene.
//! Sono anche i due rami che il messaggio sa gia' rimediare (l'uno rimanda
//! all'altro tool), quindi la natura non e' una scelta: e' quel che il testo
//! promette.
//!
//! ERRORI INGHIOTTITI CHIUSI, due `unwrap_or(None)` su altrettante query. Il
//! primo faceva di un DB irraggiungibile un «nessun profilo omonimo», e la
//! creazione proseguiva contro un DB che non risponde; il secondo faceva di un
//! DB irraggiungibile un «profilo non trovato», e l'agente veniva mandato a
//! chiamare `create_profile`, che sarebbe fallito per la stessa ragione. Ora la
//! causa e' dichiarata come [`NaturaFallimento::DelSistema`], che manda a
//! cercare un'altra strada invece di far ripetere una chiamata che rifallira'.
//!
//! La delega a sotto-agenti NON sta qui: vive in `dispatch_subagent` /
//! `dispatch_subagents`.

use nexus_types::tool_outcome::RispostaTool;
use serde_json::Value;
use sqlx::PgPool;
use uuid::Uuid;

use super::ToolContextCore;

/// L'emoji con cui nasce un profilo che non ne dichiara una. Scritta come
/// scalare Unicode perche' i sorgenti non portano emoji: e' lo stesso carattere
/// che il catalogo promette nella descrizione del campo.
const EMOJI_DEFAULT: &str = "\u{1F916}";

/// Il fallimento di una query su `user_profiles`.
///
/// DEL SISTEMA: un errore di sqlx qui e' il DB irraggiungibile, un vincolo di
/// schema o un permesso mancante — niente che l'agente possa correggere
/// riscrivendo i parametri, e ritentare identico rifallirebbe. La natura NON si
/// legge dal messaggio (regola M): a questo punto della catena l'unica cosa
/// nota e' quale domanda non ha ricevuto risposta, e quella viaggia nel testo.
fn db_fallito(cosa: &str, e: sqlx::Error) -> RispostaTool {
    RispostaTool::fallito_di_sistema(format!("[Errore: {cosa} non riuscita: {e}]"))
}

/// Un campo opzionale ridotto alla sua sostanza: spazi tolti, vuoto assente.
///
/// Conserva la semantica dell'originale — una stringa di soli spazi vale come
/// campo omesso, non come valore da scrivere.
fn ripulito(valore: Option<String>) -> Option<String> {
    valore
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
}

/// Un campo che il contratto dichiara obbligatorio e che il tool pretende anche
/// NON vuoto.
///
/// RIMEDIABILE, e il messaggio nomina il campo da riscrivere: e' esattamente
/// cio' che l'agente deve cambiare perche' la chiamata successiva passi.
fn obbligatorio_non_vuoto(valore: &str, campo: &str) -> Result<String, RispostaTool> {
    let valore = valore.trim();
    if valore.is_empty() {
        return Err(RispostaTool::fallito_rimediabile(format!(
            "[Errore: parametro '{campo}' vuoto: passa un valore non vuoto e richiama il tool]"
        )));
    }
    Ok(valore.to_string())
}

// ── Profili utente ──────────────────────────────────────────────────────────

/// I campi di `user_profiles` che la creazione scrive, gia' validati e
/// normalizzati.
///
/// Esiste perche' la validazione dei due obbligatori e la normalizzazione dei
/// cinque opzionali sono un lavoro solo: tenerlo dentro l'handler lo portava
/// oltre la soglia di lunghezza e mescolava la lettura del contratto con la
/// scrittura in DB.
struct NuovoProfilo {
    nome: String,
    system_prompt: String,
    emoji: String,
    descrizione: Option<String>,
    provider: Option<String>,
    modello: Option<String>,
    automazione: Option<String>,
    predefinito: bool,
}

impl NuovoProfilo {
    fn da_input(p: crate::tool_inputs::CreateProfileInput) -> Result<Self, RispostaTool> {
        Ok(Self {
            nome: obbligatorio_non_vuoto(&p.name, "name")?,
            system_prompt: obbligatorio_non_vuoto(&p.system_prompt, "system_prompt")?,
            emoji: ripulito(p.emoji).unwrap_or_else(|| EMOJI_DEFAULT.to_string()),
            descrizione: ripulito(p.description),
            provider: ripulito(p.default_provider),
            modello: ripulito(p.default_model),
            automazione: ripulito(p.default_automation),
            predefinito: p.set_as_default.unwrap_or(false),
        })
    }
}

/// Scrive la riga del profilo. E' l'INSERT dell'originale, invariato.
async fn inserisci_profilo(
    db: &PgPool,
    user_id: Uuid,
    profile_id: Uuid,
    p: &NuovoProfilo,
) -> Result<(), sqlx::Error> {
    sqlx::query(
        "INSERT INTO user_profiles (id, user_id, name, avatar_emoji, description, system_prompt, \
         default_provider, default_model, default_automation, is_default, created_at, updated_at) \
         VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, NOW(), NOW())",
    )
    .bind(profile_id)
    .bind(user_id)
    .bind(&p.nome)
    .bind(&p.emoji)
    .bind(&p.descrizione)
    .bind(&p.system_prompt)
    .bind(&p.provider)
    .bind(&p.modello)
    .bind(&p.automazione)
    .bind(p.predefinito)
    .execute(db)
    .await
    .map(|_| ())
}

pub async fn tool_create_profile(ctx: &ToolContextCore, input: &Value) -> RispostaTool {
    use crate::{input_contract::InputTool, tool_inputs::CreateProfileInput};

    let params = match CreateProfileInput::leggi(input) {
        Ok(p) => p,
        Err(risposta) => return risposta,
    };
    let profilo = match NuovoProfilo::da_input(params) {
        Ok(p) => p,
        Err(risposta) => return risposta,
    };

    // Il nome omonimo si cerca PRIMA di scrivere. L'`unwrap_or(None)` storico
    // trattava il silenzio del DB come «nessun omonimo»: la creazione andava
    // avanti verso un DB che non risponde, e il fallimento arrivava una query
    // dopo con un'altra causa scritta sopra.
    let esistente: Option<(Uuid,)> =
        match sqlx::query_as("SELECT id FROM user_profiles WHERE user_id = $1 AND name = $2")
            .bind(ctx.user_id)
            .bind(&profilo.nome)
            .fetch_optional(&*ctx.db)
            .await
        {
            Ok(riga) => riga,
            Err(e) => return db_fallito("la verifica del nome del profilo", e),
        };
    if esistente.is_some() {
        let nome = &profilo.nome;
        return RispostaTool::fallito_rimediabile(format!(
            "[Profilo '{nome}' gia' esistente: nessun profilo creato. Chiama update_profile con \
             profile_name='{nome}' per modificarlo, oppure ripeti con un 'name' diverso.]"
        ));
    }

    // Il profilo predefinito e' uno solo: se l'azzeramento degli altri fallisce
    // NON si inserisce, o il DB resterebbe con due predefiniti. Fermarsi qui
    // lascia il DB come lo si e' trovato.
    if profilo.predefinito {
        let reset = sqlx::query("UPDATE user_profiles SET is_default = FALSE WHERE user_id = $1")
            .bind(ctx.user_id)
            .execute(&*ctx.db)
            .await;
        if let Err(e) = reset {
            return db_fallito("la revoca del profilo predefinito precedente", e);
        }
    }

    let profile_id = Uuid::new_v4();
    match inserisci_profilo(&ctx.db, ctx.user_id, profile_id, &profilo).await {
        Ok(()) => RispostaTool::riuscito(format!(
            "Profilo '{}' {} creato con successo (ID: {profile_id}). L'utente lo trovera' nel \
             selettore profili accanto alla chat.",
            profilo.nome, profilo.emoji
        )),
        Err(e) => db_fallito("la creazione del profilo", e),
    }
}

/// Trova il profilo da aggiornare, coi valori attuali dei due campi che
/// l'aggiornamento puo' lasciare invariati.
///
/// I due esiti non riusciti restano DISTINTI perche' hanno nature diverse: il
/// DB muto e' fuori dalla portata dell'agente, il nome che non esiste lo
/// rimedia riscrivendolo o chiamando `create_profile`. L'`unwrap_or(None)`
/// storico li appiattiva sullo stesso ramo, e il primo veniva raccontato come
/// il secondo.
async fn profilo_da_aggiornare(
    ctx: &ToolContextCore,
    profile_name: &str,
) -> Result<(Uuid, String, String), RispostaTool> {
    let riga: Option<(Uuid, String, String)> = sqlx::query_as(
        "SELECT id, system_prompt, avatar_emoji FROM user_profiles WHERE user_id = $1 AND name = $2",
    )
    .bind(ctx.user_id)
    .bind(profile_name)
    .fetch_optional(&*ctx.db)
    .await
    .map_err(|e| db_fallito("la ricerca del profilo", e))?;
    riga.ok_or_else(|| {
        RispostaTool::fallito_rimediabile(format!(
            "[Profilo '{profile_name}' non trovato: nessun aggiornamento. Il nome deve \
             corrispondere esattamente a quello del profilo; usa create_profile per crearlo.]"
        ))
    })
}

pub async fn tool_update_profile(ctx: &ToolContextCore, input: &Value) -> RispostaTool {
    use crate::{input_contract::InputTool, tool_inputs::UpdateProfileInput};

    let params = match UpdateProfileInput::leggi(input) {
        Ok(p) => p,
        Err(risposta) => return risposta,
    };
    let profile_name = match obbligatorio_non_vuoto(&params.profile_name, "profile_name") {
        Ok(nome) => nome,
        Err(risposta) => return risposta,
    };
    let system_prompt = ripulito(params.system_prompt);
    let emoji = ripulito(params.emoji);
    let descrizione = ripulito(params.description);

    // Tutti e tre gli aggiornamenti sono opzionali nel contratto, ma una
    // chiamata che non ne porta nessuno riscriveva il profilo coi valori che
    // aveva gia' e dichiarava «aggiornato con successo»: un lavoro annunciato e
    // non fatto. RIMEDIABILE, e il messaggio elenca i campi che lo rimediano.
    if system_prompt.is_none() && emoji.is_none() && descrizione.is_none() {
        return RispostaTool::fallito_rimediabile(
            "[Errore: nessun aggiornamento richiesto: insieme a 'profile_name' passa almeno uno \
             fra 'system_prompt', 'description' ed 'emoji']",
        );
    }

    // Il ramo «non trovato» era l'esito peggiore dell'`unwrap_or(None)` storico:
    // un DB muto diventava un profilo inesistente, e il messaggio mandava
    // l'agente su create_profile — cioe' su una scrittura che sarebbe fallita
    // per la stessa causa, questa volta dopo aver toccato il DB.
    let (profile_id, prompt_attuale, emoji_attuale) =
        match profilo_da_aggiornare(ctx, &profile_name).await {
            Ok(riga) => riga,
            Err(risposta) => return risposta,
        };

    let res = sqlx::query(
        "UPDATE user_profiles SET system_prompt = $1, avatar_emoji = $2, \
         description = COALESCE($3, description), updated_at = NOW() WHERE id = $4",
    )
    .bind(system_prompt.unwrap_or(prompt_attuale))
    .bind(emoji.unwrap_or(emoji_attuale))
    .bind(&descrizione)
    .bind(profile_id)
    .execute(&*ctx.db)
    .await;

    match res {
        Ok(_) => RispostaTool::riuscito(format!("Profilo '{profile_name}' aggiornato con successo.")),
        Err(e) => db_fallito("l'aggiornamento del profilo", e),
    }
}
