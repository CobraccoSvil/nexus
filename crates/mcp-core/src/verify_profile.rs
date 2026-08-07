//! Profilo di verifica per-progetto INFERITO DA LLM (ADR 0036, mig 0508).
//!
//! Problema risolto (caso reale Beaty-Book): il final_gate validava con un
//! generico "npm run build" — per un progetto Vite la build non fa type-check
//! e un run con import/export rotti veniva chiuso "Verifica superata".
//!
//! Principio (voluto esplicitamente): NIENTE enumerazioni fisse. Non una
//! lista di manifest riconosciuti, non un vocabolario di step, non una
//! matrice linguaggio->comando: e' l'LLM che
//!   1. sceglie QUALI file leggere dal listing del progetto (pass 1);
//!   2. definisce la catena di verifica con step dal NOME LIBERO, ciascuno
//!      col proprio comando e col flag `gate` (eseguirlo nella verifica di
//!      chiusura del run) (pass 2).
//! Fisso resta solo cio' che e' sicurezza o determinismo:
//!   - ogni comando passa dal punto unico di safety
//!     (`nexus_agent_tools::safety::check_command`, regola L) — mai eseguito
//!     un comando bloccato;
//!   - i file richiesti dall'LLM sono letti SOLO sotto la root del progetto,
//!     in numero e dimensione limitati;
//!   - l'invalidazione della cache e' deterministica (hash del listing + dei
//!      file osservati), MAI decisa a runtime da un LLM.
//! Esito STRUTTURATO (JSON, regola M), persistito in `project_verify_profiles`
//! (meta-DB): una inferenza per progetto, rigenerata solo quando l'ambiente
//! cambia. Modello via purpose `verify_infer` (regola G), prompt nel registry
//! (`system.verify_infer.select_files` / `system.verify_infer.infer_chain`).
//!
//! Precedenza d'uso: `run_configurations` (override utente) > profilo LLM >
//! matrice statica `agent.verify.<lang>.<step>` (rete di sicurezza).
//! L'inferenza parte SOLO dal run nativo; i lettori (tool, resolver) leggono
//! la tabella e degradano alla rete statica se il profilo non c'e' ancora.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use sqlx::PgPool;
use uuid::Uuid;

use crate::orchestrator::NeuralCoreClient;

/// Uno step del profilo, come persistito in `project_verify_profiles.steps`.
/// `step` e' un nome LIBERO deciso dall'LLM per quell'ambiente (es.
/// "typecheck", "schema-validate", "container-build"): nessun vocabolario.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct VerifyProfileStep {
    pub step: String,
    pub command: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub working_dir: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub timeout_s: Option<f64>,
    /// `true` = lo step fa parte della verifica di CHIUSURA del run
    /// (final_gate). Decide l'LLM: il gate deve restare rapido ma completo
    /// per QUELL'ambiente (es. typecheck+build si', suite E2E no).
    #[serde(default)]
    pub gate: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rationale: Option<String>,
    /// Exit code dello step misurato sull'albero PRE-LAVORO (backfill lazy nel
    /// punto d'innesto del run primario, MAI generato dall'LLM). Baseline del
    /// gate delta-aware sui criteri: un fallimento IDENTICO alla baseline
    /// (stesso exit non-zero, zero file localizzati) e' debito pre-esistente
    /// dell'ambiente e non boccia il run. Additivo (`serde default`):
    /// retro-compatibile coi profili gia' persistiti; una re-inferenza produce
    /// step senza baseline -> ri-misura automatica al run successivo.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub baseline_exit_code: Option<i64>,
    /// Esito della PROVA DI EFFICACIA dello step (`verify_probe`): il comando
    /// sa fallire? Misurato una tantum accanto alla baseline, MAI generato
    /// dall'LLM. `None` = non ancora provato (ri-misura al run successivo).
    ///
    /// Serve perche' il flag `gate` qui sopra e' una DICHIARAZIONE del modello e
    /// nessuno verificava che il comando fosse capace di bocciare qualcosa: uno
    /// step `Blind` non e' una verifica e non deve contare come tale (l'esito
    /// onesto e' `CompletedUnverified`, non un "completato" muto).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub probe: Option<crate::verify_probe::ProbeOutcome>,
}

/// Limiti di sicurezza dell'osservazione (bounded, non "intelligenza"):
/// quante voci di listing mostrare, quanti file lasciar leggere all'LLM e
/// quanto grande puo' essere ciascun estratto.
const LISTING_MAX_ENTRIES: usize = 300;
const FILES_MAX_COUNT: usize = 15;
const FILE_MAX_CHARS: usize = 6000;

/// Directory di rumore escluse dal listing (artefatti, mai segnale).
const NOISE_DIRS: [&str; 8] = [
    "node_modules",
    "target",
    ".git",
    ".next",
    "dist",
    "build",
    "__pycache__",
    ".venv",
];

/// Listing shallow (root + primo livello) del progetto: i NOMI parlano
/// all'LLM, i contenuti arrivano solo per i file che LUI chiede (pass 1).
/// Bloccante: chiamare via `spawn_blocking`.
pub fn project_listing(root: &Path) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    let Ok(entries) = std::fs::read_dir(root) else {
        return out;
    };
    let mut sorted: Vec<_> = entries.flatten().collect();
    sorted.sort_by_key(|e| e.file_name());
    for entry in sorted {
        if out.len() >= LISTING_MAX_ENTRIES {
            break;
        }
        let name = entry.file_name().to_string_lossy().to_string();
        if NOISE_DIRS.contains(&name.as_str()) {
            continue;
        }
        let path = entry.path();
        if path.is_dir() {
            out.push(format!("{name}/"));
            if let Ok(children) = std::fs::read_dir(&path) {
                let mut kids: Vec<_> = children.flatten().collect();
                kids.sort_by_key(|e| e.file_name());
                for kid in kids {
                    if out.len() >= LISTING_MAX_ENTRIES {
                        break;
                    }
                    let kname = kid.file_name().to_string_lossy().to_string();
                    if NOISE_DIRS.contains(&kname.as_str()) {
                        continue;
                    }
                    let suffix = if kid.path().is_dir() { "/" } else { "" };
                    out.push(format!("{name}/{kname}{suffix}"));
                }
            }
        } else {
            out.push(name);
        }
    }
    out
}

/// Risolve un path RELATIVO richiesto dall'LLM in modo confinato alla root:
/// niente assoluti, niente `..`, il canonico deve restare sotto la root.
/// Ritorna `None` (file ignorato) per qualunque violazione.
fn confine_to_root(root: &Path, requested: &str) -> Option<PathBuf> {
    let rel = requested.trim().replace('\\', "/");
    if rel.is_empty() || rel.starts_with('/') || rel.contains("..") || rel.contains(':') {
        return None;
    }
    let candidate = root.join(&rel);
    let canon = candidate.canonicalize().ok()?;
    let root_canon = root.canonicalize().ok()?;
    canon.starts_with(&root_canon).then_some(canon)
}

/// Legge (bounded) i file scelti dall'LLM. Ritorna coppie (path relativo,
/// contenuto troncato). Bloccante: chiamare via `spawn_blocking`.
pub fn read_requested_files(root: &Path, requested: &[String]) -> Vec<(String, String)> {
    let mut out = Vec::new();
    for req in requested.iter().take(FILES_MAX_COUNT) {
        let Some(path) = confine_to_root(root, req) else {
            tracing::warn!(requested = %req, "verify_profile: path richiesto dall'LLM fuori dalla root, ignorato");
            continue;
        };
        let Ok(content) = std::fs::read_to_string(&path) else {
            continue; // binario o illeggibile: nessun segnale testuale.
        };
        let excerpt: String = content.chars().take(FILE_MAX_CHARS).collect();
        out.push((req.trim().replace('\\', "/"), excerpt));
    }
    out
}

/// Hash DETERMINISTICO dell'ambiente osservato: listing (nomi) + contenuto
/// integrale dei file osservati nell'ultima inferenza. Un file nuovo in
/// root/primo livello cambia il listing -> re-inferenza; una modifica a un
/// file osservato cambia il contenuto -> re-inferenza. Nessun LLM qui.
pub fn environment_hash(root: &Path, listing: &[String], observed_files: &[String]) -> String {
    let mut hasher = Sha256::new();
    for l in listing {
        hasher.update(l.as_bytes());
        hasher.update(b"\n");
    }
    for f in observed_files {
        if let Some(p) = confine_to_root(root, f) {
            if let Ok(content) = std::fs::read_to_string(&p) {
                hasher.update(f.as_bytes());
                hasher.update(content.as_bytes());
            }
        }
    }
    format!("{:x}", hasher.finalize())
}

/// Persiste gli step aggiornati (es. backfill delle baseline pre-lavoro) sul
/// profilo esistente. UPDATE puro del JSONB: non tocca environment/hash/source,
/// quindi non altera il cache-hit dell'inferenza. Idempotente sotto
/// concorrenza (due primari misurano la stessa baseline).
pub async fn persist_steps(meta_db: &PgPool, project_id: Uuid, steps: &[VerifyProfileStep]) {
    let Ok(json) = serde_json::to_value(steps) else {
        return;
    };
    if let Err(e) = sqlx::query(
        "UPDATE project_verify_profiles SET steps = $2, updated_at = now() WHERE project_id = $1",
    )
    .bind(project_id)
    .bind(json)
    .execute(meta_db)
    .await
    {
        tracing::warn!(error = %e, %project_id, "verify_profile: persist_steps fallita (baseline non salvata)");
    }
}

/// Legge gli step del profilo persistito (lettura pura, nessuna inferenza).
pub async fn profile_steps(meta_db: &PgPool, project_id: Uuid) -> Vec<VerifyProfileStep> {
    let row: Option<serde_json::Value> =
        sqlx::query_scalar("SELECT steps FROM project_verify_profiles WHERE project_id = $1")
            .bind(project_id)
            .fetch_optional(meta_db)
            .await
            .ok()
            .flatten();
    row.and_then(|v| serde_json::from_value(v).ok())
        .unwrap_or_default()
}

/// Valida gli step proposti dall'LLM: nome non vuoto (bounded), comando non
/// vuoto e AMMESSO dal punto unico di safety. Gli scarti sono loggati, mai
/// eseguiti. L'ORDINE e' quello proposto dall'LLM (nessun vocabolario).
fn validate_steps(parsed: &serde_json::Value) -> Option<Vec<VerifyProfileStep>> {
    let steps_raw = parsed.get("steps")?.as_array()?.clone();
    let mut steps: Vec<VerifyProfileStep> = Vec::new();
    for s in steps_raw {
        let Ok(mut step) = serde_json::from_value::<VerifyProfileStep>(s) else {
            tracing::warn!("verify_profile: step LLM non deserializzabile, scartato");
            continue;
        };
        step.step = step.step.trim().chars().take(48).collect();
        if step.step.is_empty() || step.command.trim().is_empty() {
            continue;
        }
        if let Some(reason) = nexus_agent_tools::safety::check_command(&step.command) {
            tracing::warn!(
                step = %step.step,
                category = %reason.category,
                "verify_profile: comando proposto dall'LLM bloccato dalla safety, scartato"
            );
            continue;
        }
        steps.push(step);
    }
    (!steps.is_empty()).then_some(steps)
}

/// Una chiamata LLM del flusso di inferenza (system dal registry + user).
async fn infer_call(
    neural: &NeuralCoreClient,
    provider: &str,
    model: &str,
    system_text: &str,
    user_text: String,
    timeout_s: u64,
) -> Option<serde_json::Value> {
    let messages = serde_json::json!([{ "role": "user", "content": user_text }]).to_string();
    let resp = tokio::time::timeout(
        std::time::Duration::from_secs(timeout_s),
        neural.generate_agent_turn(provider, model, &messages, "[]", 1500, system_text),
    )
    .await;
    let value = match resp {
        Ok(Ok(v)) => v,
        Ok(Err(e)) => {
            tracing::warn!(error = %e, "verify_profile: chiamata di inferenza fallita");
            return None;
        }
        Err(_) => {
            tracing::warn!(timeout_s, "verify_profile: inferenza oltre il timeout");
            return None;
        }
    };
    let text = value
        .get("content")
        .and_then(|v| v.as_str())
        .or_else(|| value.get("text").and_then(|v| v.as_str()))
        .unwrap_or("");
    nexus_types::llm_json::extract_json_block(text)
}

/// Single-flight per `project_id` di [`ensure_profile`] (regola L, stessa forma
/// di `PROVISION_LOCKS` in `project_db_routes::provision`).
///
/// Perche' esiste (incidente consiglio 2026-07-15, difetto D1 PROVATO): il
/// consiglio convoca 6 figure che entrano CONTEMPORANEAMENTE in `run_native`,
/// e ognuna chiamava `ensure_profile` sullo STESSO progetto -> fino a 6
/// inferenze LLM IDENTICHE e duplicate (2 chiamate ciascuna: scelta file +
/// catena), tutte in volo insieme. E' il lavoro che occupava la finestra dei
/// ~119s prima che le figure arrivassero al loro modello.
///
/// Il guard e' un `tokio::sync::Mutex` per progetto (mai attraverso un lock
/// sincrono tenuto su un `.await`): il PRIMO arrivato infersce, gli altri
/// attendono e poi ri-leggono il PERSISTITO — che a quel punto e' fresco, e
/// l'hash deterministico del passo 2 li fa ritornare subito senza LLM. Cosi'
/// il risultato e' CONDIVISO invece che ricalcolato (il piano avverte: un lock
/// che serializza N inferenze invece di condividerne una sarebbe solo un altro
/// difetto).
static PROFILE_LOCKS: once_cell::sync::Lazy<
    std::sync::Mutex<std::collections::HashMap<Uuid, std::sync::Arc<tokio::sync::Mutex<()>>>>,
> = once_cell::sync::Lazy::new(|| std::sync::Mutex::new(std::collections::HashMap::new()));

/// Guard single-flight per `project_id`.
fn profile_lock(project_id: Uuid) -> std::sync::Arc<tokio::sync::Mutex<()>> {
    let mut map = PROFILE_LOCKS
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    map.entry(project_id)
        .or_insert_with(|| std::sync::Arc::new(tokio::sync::Mutex::new(())))
        .clone()
}

/// Guard di MUTUA ESCLUSIONE sull'ALBERO di un progetto (difetto D2, incidente
/// consiglio 2026-07-15). Chi misura la baseline o esegue la prova di efficacia
/// (`verify_probe::probe_step`, che PIANTA un file sintetico rotto nel working
/// tree e ri-esegue il comando) deve averlo in esclusiva: due misure
/// sovrapposte sullo stesso albero si corrompono a vicenda e producono
/// `Blind`/`Discriminating` casuali. La chiave e' la ROOT (la risorsa
/// condivisa e' l'albero, non la riga di progetto).
static TREE_LOCKS: once_cell::sync::Lazy<
    std::sync::Mutex<std::collections::HashMap<String, std::sync::Arc<tokio::sync::Mutex<()>>>>,
> = once_cell::sync::Lazy::new(|| std::sync::Mutex::new(std::collections::HashMap::new()));

/// Guard esclusivo dell'albero `root` (vedi [`TREE_LOCKS`]).
pub(crate) fn project_tree_lock(root: &str) -> std::sync::Arc<tokio::sync::Mutex<()>> {
    let mut map = TREE_LOCKS
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    map.entry(root.to_string())
        .or_insert_with(|| std::sync::Arc::new(tokio::sync::Mutex::new(())))
        .clone()
}

/// Garantisce un profilo di verifica AGGIORNATO per il progetto e ne ritorna
/// gli step. Flusso:
///   1. kill-switch `agent.verify_infer.enabled`;
///   2. hash deterministico su listing + file osservati dall'ULTIMA
///      inferenza: se coincide col persistito -> ritorno immediato (zero LLM);
///   3. pass 1 — l'LLM sceglie dal listing quali file leggere;
///   4. pass 2 — l'LLM produce la catena (step liberi, flag `gate`);
///   5. validazione safety + persistenza (steps + observed_files + hash).
/// Best-effort: ogni errore ritorna gli step correnti (anche stale) o vuoto,
/// con WARN — il final_gate degrada alla rete statica, mai un blocco del run.
///
/// SINGLE-FLIGHT per progetto (vedi [`PROFILE_LOCKS`]): N chiamanti concorrenti
/// producono UNA sola inferenza; i successivi ritrovano il profilo fresco e
/// ritornano al passo 2 senza chiamare l'LLM.
pub async fn ensure_profile(
    meta_db: &PgPool,
    neural: &NeuralCoreClient,
    project_id: Uuid,
    root: &Path,
) -> Vec<VerifyProfileStep> {
    let lock = profile_lock(project_id);
    let _guard = lock.lock().await;
    ensure_profile_locked(meta_db, neural, project_id, root).await
}

/// Corpo di [`ensure_profile`], gia' sotto il guard single-flight del progetto.
async fn ensure_profile_locked(
    meta_db: &PgPool,
    neural: &NeuralCoreClient,
    project_id: Uuid,
    root: &Path,
) -> Vec<VerifyProfileStep> {
    let enabled =
        nexus_auth::get_bool_setting_or(meta_db, "agent.verify_infer.enabled", false).await;
    if !enabled {
        return Vec::new();
    }

    // Stato persistito: steps + file osservati + hash + ownership.
    let cached: Option<(serde_json::Value, serde_json::Value, String, String)> = sqlx::query_as(
        "SELECT steps, environment, manifest_hash, source \
             FROM project_verify_profiles WHERE project_id = $1",
    )
    .bind(project_id)
    .fetch_optional(meta_db)
    .await
    .ok()
    .flatten();
    let cached_steps = |c: &Option<(serde_json::Value, serde_json::Value, String, String)>| {
        c.as_ref()
            .and_then(|(s, _, _, _)| serde_json::from_value(s.clone()).ok())
            .unwrap_or_default()
    };
    // Profilo impostato a mano: override esplicito, mai sovrascritto dall'LLM.
    if let Some((_, _, _, source)) = &cached {
        if source == "user" {
            return cached_steps(&cached);
        }
    }

    let root_owned = root.to_path_buf();
    let Ok(listing) = tokio::task::spawn_blocking(move || project_listing(&root_owned)).await
    else {
        return cached_steps(&cached);
    };
    if listing.is_empty() {
        return cached_steps(&cached);
    }

    // Cache-hit deterministico: hash su listing + file osservati l'ultima volta.
    if let Some((_, environment, hash, _)) = &cached {
        let observed: Vec<String> = environment
            .get("observed_files")
            .and_then(|v| serde_json::from_value(v.clone()).ok())
            .unwrap_or_default();
        let root_owned = root.to_path_buf();
        let listing_cl = listing.clone();
        let current = tokio::task::spawn_blocking(move || {
            environment_hash(&root_owned, &listing_cl, &observed)
        })
        .await
        .unwrap_or_default();
        if current == *hash {
            return cached_steps(&cached);
        }
    }

    // Modello dal purpose (regola G) e prompt dal registry (regola D/G).
    let (provider, model) = match crate::internal_routing::resolve_purpose_model_db(
        meta_db,
        "verify_infer",
    )
    .await
    .into_model("verify_infer")
    {
        Ok(pm) => pm,
        Err(e) => {
            tracing::warn!(error = %e, "verify_profile: purpose verify_infer non risolvibile, degrado alla rete statica");
            return cached_steps(&cached);
        }
    };
    // Cache template monouso: l'inferenza gira solo su cache-miss (rara), il
    // punto unico del loader resta rispettato (regola L).
    let tpl_cache = crate::prompt_templates::TemplateCache::new();
    let select_tpl = crate::prompt_templates::get_template_or_default(
        meta_db,
        &tpl_cache,
        "system.verify_infer.select_files",
    )
    .await;
    let infer_tpl = crate::prompt_templates::get_template_or_default(
        meta_db,
        &tpl_cache,
        "system.verify_infer.infer_chain",
    )
    .await;
    if select_tpl.trim().is_empty() || infer_tpl.trim().is_empty() {
        tracing::warn!("verify_profile: template verify_infer assenti, degrado alla rete statica");
        return cached_steps(&cached);
    }
    let timeout_s = nexus_auth::get_setting(meta_db, "agent.verify_infer.timeout_s")
        .await
        .and_then(|v| v.trim().parse::<u64>().ok())
        .unwrap_or(45)
        .clamp(5, 300);

    // ── Pass 1: l'LLM sceglie cosa osservare ────────────────────────────────
    let listing_text = listing.join("\n");
    let Some(selection) = infer_call(
        neural,
        &provider,
        &model,
        &select_tpl,
        format!("Listing del progetto (root + primo livello):\n{listing_text}"),
        timeout_s,
    )
    .await
    else {
        return cached_steps(&cached);
    };
    let requested: Vec<String> = selection
        .get("files_to_read")
        .and_then(|v| serde_json::from_value(v.clone()).ok())
        .unwrap_or_default();
    if requested.is_empty() {
        tracing::warn!("verify_profile: pass 1 senza file richiesti, degrado alla rete statica");
        return cached_steps(&cached);
    }

    let root_owned = root.to_path_buf();
    let req_cl = requested.clone();
    let Ok(files) =
        tokio::task::spawn_blocking(move || read_requested_files(&root_owned, &req_cl)).await
    else {
        return cached_steps(&cached);
    };
    let observed_files: Vec<String> = files.iter().map(|(p, _)| p.clone()).collect();

    // ── Pass 2: l'LLM produce la catena per QUESTO ambiente ────────────────
    let files_block = files
        .iter()
        .map(|(p, c)| format!("=== {p} ===\n{c}"))
        .collect::<Vec<_>>()
        .join("\n\n");
    let Some(chain) = infer_call(
        neural,
        &provider,
        &model,
        &infer_tpl,
        format!(
            "Listing del progetto:\n{listing_text}\n\nContenuto dei file che hai chiesto:\n{files_block}"
        ),
        timeout_s,
    )
    .await
    else {
        return cached_steps(&cached);
    };
    let Some(steps) = validate_steps(&chain) else {
        tracing::warn!("verify_profile: pass 2 senza step validi, degrado alla rete statica");
        return cached_steps(&cached);
    };
    let env_summary = chain
        .get("environment_summary")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();

    // Hash sul NUOVO insieme osservato (listing + file letti ora).
    let root_owned = root.to_path_buf();
    let listing_cl = listing.clone();
    let observed_cl = observed_files.clone();
    let new_hash = tokio::task::spawn_blocking(move || {
        environment_hash(&root_owned, &listing_cl, &observed_cl)
    })
    .await
    .unwrap_or_default();

    let steps_json = serde_json::to_value(&steps).unwrap_or_else(|_| serde_json::json!([]));
    let environment = serde_json::json!({
        "summary": env_summary,
        "observed_files": observed_files,
    });
    let res = sqlx::query(
        "INSERT INTO project_verify_profiles \
             (project_id, steps, environment, manifest_hash, source, provider, model, updated_at) \
         VALUES ($1, $2, $3, $4, 'llm', $5, $6, NOW()) \
         ON CONFLICT (project_id) DO UPDATE SET \
             steps = EXCLUDED.steps, environment = EXCLUDED.environment, \
             manifest_hash = EXCLUDED.manifest_hash, source = 'llm', \
             provider = EXCLUDED.provider, model = EXCLUDED.model, updated_at = NOW()",
    )
    .bind(project_id)
    .bind(&steps_json)
    .bind(&environment)
    .bind(&new_hash)
    .bind(&provider)
    .bind(&model)
    .execute(meta_db)
    .await;
    if let Err(e) = res {
        tracing::warn!(error = %e, "verify_profile: persistenza profilo fallita (best-effort)");
    } else {
        tracing::info!(
            %project_id,
            steps = steps.len(),
            provider = %provider,
            model = %model,
            "verify_profile: profilo di verifica inferito dall'ambiente e persistito"
        );
    }
    steps
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn validate_accetta_step_liberi_e_scarta_gli_invalidi() {
        let parsed = json!({"steps":[
            {"step":"schema-validate","command":"npx prisma validate","gate":true},
            {"step":"typecheck","command":"npx tsc --noEmit","gate":true},
            {"step":"","command":"echo x"},
            {"step":"danger","command":"rm -rf /etc/nginx"},
            {"step":"e2e","command":"npx playwright test","gate":false}
        ]});
        let steps = validate_steps(&parsed).expect("step validi");
        let names: Vec<&str> = steps.iter().map(|s| s.step.as_str()).collect();
        // Ordine dell'LLM preservato (nessun vocabolario, nessun sort).
        assert_eq!(names, vec!["schema-validate", "typecheck", "e2e"]);
        assert!(steps[0].gate && steps[1].gate && !steps[2].gate);
    }

    #[test]
    fn validate_ritorna_none_senza_step() {
        assert!(validate_steps(&json!({"steps":[]})).is_none());
        assert!(validate_steps(&json!({})).is_none());
    }

    #[test]
    fn confine_rifiuta_path_fuori_root() {
        let dir = tempfile::tempdir().expect("tempdir");
        std::fs::write(dir.path().join("ok.txt"), "x").expect("write");
        assert!(confine_to_root(dir.path(), "ok.txt").is_some());
        assert!(confine_to_root(dir.path(), "../fuori.txt").is_none());
        assert!(confine_to_root(dir.path(), "/etc/passwd").is_none());
        assert!(confine_to_root(dir.path(), "C:/Windows/win.ini").is_none());
        assert!(confine_to_root(dir.path(), "manca.txt").is_none());
    }

    #[test]
    fn hash_deterministico_e_sensibile_a_listing_e_contenuti() {
        let dir = tempfile::tempdir().expect("tempdir");
        std::fs::write(dir.path().join("package.json"), r#"{"a":1}"#).expect("write");
        let listing = project_listing(dir.path());
        let observed = vec!["package.json".to_string()];
        let a = environment_hash(dir.path(), &listing, &observed);
        let b = environment_hash(dir.path(), &listing, &observed);
        assert_eq!(a, b, "deterministico");
        // Contenuto osservato cambia -> hash cambia.
        std::fs::write(dir.path().join("package.json"), r#"{"a":2}"#).expect("write");
        let c = environment_hash(dir.path(), &listing, &observed);
        assert_ne!(a, c);
        // File NUOVO in root -> listing cambia -> hash cambia (anche se non
        // era tra gli osservati: e' il segnale di ri-inferenza).
        std::fs::write(dir.path().join("Cargo.toml"), "[package]").expect("write");
        let listing2 = project_listing(dir.path());
        let d = environment_hash(dir.path(), &listing2, &observed);
        assert_ne!(c, d);
    }

    #[test]
    fn read_requested_limita_e_confina() {
        let dir = tempfile::tempdir().expect("tempdir");
        std::fs::write(dir.path().join("a.json"), "{}").expect("write");
        let files = read_requested_files(
            dir.path(),
            &[
                "a.json".to_string(),
                "../evil".to_string(),
                "manca.txt".to_string(),
            ],
        );
        assert_eq!(files.len(), 1);
        assert_eq!(files[0].0, "a.json");
    }

    // ── Single-flight (difetto D1, incidente consiglio 2026-07-15) ───────────

    /// Il guard e' PER PROGETTO: due progetti diversi non si serializzano
    /// (altrimenti il single-flight diventerebbe un collo di bottiglia globale).
    #[test]
    fn profile_lock_e_per_progetto() {
        let a = Uuid::new_v4();
        let b = Uuid::new_v4();
        assert!(
            std::sync::Arc::ptr_eq(&profile_lock(a), &profile_lock(a)),
            "stesso progetto -> STESSO guard (altrimenti niente single-flight)"
        );
        assert!(
            !std::sync::Arc::ptr_eq(&profile_lock(a), &profile_lock(b)),
            "progetti diversi -> guard diversi (nessuna serializzazione globale)"
        );
    }

    /// EFFETTO del single-flight: N chiamanti concorrenti sullo stesso progetto
    /// producono UNA sola sezione critica alla volta, e il secondo entra solo
    /// dopo che il primo ha finito (nel path reale: trova il profilo persistito
    /// e ritorna senza inferenza). Il test misura la MUTUA ESCLUSIONE, non la
    /// presenza del lock: un contatore di ingressi concorrenti che superi 1
    /// significherebbe inferenze duplicate — il difetto D1.
    #[tokio::test]
    async fn single_flight_serializza_i_concorrenti_dello_stesso_progetto() {
        use std::sync::atomic::{AtomicUsize, Ordering};
        use std::sync::Arc;
        let pid = Uuid::new_v4();
        let dentro = Arc::new(AtomicUsize::new(0));
        let max_visti = Arc::new(AtomicUsize::new(0));
        let ingressi = Arc::new(AtomicUsize::new(0));
        let mut handles = Vec::new();
        for _ in 0..6 {
            let (dentro, max_visti, ingressi) =
                (dentro.clone(), max_visti.clone(), ingressi.clone());
            handles.push(tokio::spawn(async move {
                let lock = profile_lock(pid);
                let _g = lock.lock().await;
                ingressi.fetch_add(1, Ordering::SeqCst);
                let ora = dentro.fetch_add(1, Ordering::SeqCst) + 1;
                max_visti.fetch_max(ora, Ordering::SeqCst);
                // Simula l'inferenza LLM (nel path reale: ~secondi).
                tokio::time::sleep(std::time::Duration::from_millis(20)).await;
                dentro.fetch_sub(1, Ordering::SeqCst);
            }));
        }
        for h in handles {
            h.await.expect("task");
        }
        assert_eq!(
            max_visti.load(Ordering::SeqCst),
            1,
            "MAI due inferenze dello stesso progetto in volo insieme (D1: erano 6)"
        );
        assert_eq!(ingressi.load(Ordering::SeqCst), 6, "tutti devono passare");
    }

    /// Il guard dell'ALBERO (difetto D2) e' per ROOT: la risorsa condivisa e'
    /// il working tree su cui il probe pianta il file sintetico.
    #[test]
    fn tree_lock_e_per_root() {
        assert!(
            std::sync::Arc::ptr_eq(
                &project_tree_lock("D:/progetti/alfa"),
                &project_tree_lock("D:/progetti/alfa")
            ),
            "stessa root -> stesso guard (il probe muta l'albero: esclusione)"
        );
        assert!(
            !std::sync::Arc::ptr_eq(
                &project_tree_lock("D:/progetti/alfa"),
                &project_tree_lock("D:/progetti/beta")
            ),
            "root diverse -> guard diversi"
        );
    }
}
