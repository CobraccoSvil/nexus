//! Tool Git: status, stage, commit, push, pull.
//!
//! MIGRATI al contratto d'ingresso e a `RispostaTool` (regola Q).
//!
//! DIVERGENZA CHIUSA, e non era piccola: CINQUE handler su sei componevano il
//! proprio errore come `[git <verbo> error: ...]`, SENZA il marker di
//! fallimento. Un `git commit` che rifiuta per hook, un `git push` respinto dal
//! remoto, un `git pull --rebase` fermo su conflitto arrivavano all'agente come
//! esecuzioni RIUSCITE il cui testo raccontava un errore — e la regola M vieta
//! di leggere quel testo per accorgersene. L'agente proseguiva come se il lavoro
//! fosse salvato.
//!
//! Il caso e' quello che la regola Q descrive alla lettera: finche' la firma era
//! `-> String` l'esito non aveva dove stare, e comporre `format!("[git ... ]")`
//! era indistinguibile dal comporre un risultato. Col campo la dimenticanza non
//! e' piu' rappresentabile.

use nexus_types::tool_outcome::RispostaTool;
use nexus_types::git_exec::run_git_command;
use serde_json::Value;

use super::ToolContextCore;

/// Il rifiuto dei tool git quando il progetto non e' un repository.
///
/// DEL SISTEMA: non e' un parametro che l'agente possa correggere ne' una
/// condizione che passi da sola. Prima usciva NUDO — cioe' come un successo —
/// da tutti e sei gli handler.
fn non_e_un_repo() -> RispostaTool {
    RispostaTool::fallito_di_sistema(
        "Il progetto non e' un repository git: nessun comando git e' applicabile.",
    )
}

/// Il rifiuto per permesso di scrittura mancante.
///
/// DEL SISTEMA: e' una decisione del progetto sul run, non un parametro della
/// chiamata. Ripetere non la cambia.
fn permesso_di_scrittura(ctx: &ToolContextCore) -> Option<RispostaTool> {
    if ctx.can_write {
        return None;
    }
    Some(RispostaTool::fallito_di_sistema(
        "[Errore: permesso di scrittura non concesso]",
    ))
}

/// Il fallimento di un comando git.
///
/// DEL SISTEMA per default e non rimediabile: `run_git_command` fallisce per lo
/// stato del repository o del remoto (hook che rifiuta, conflitto, credenziali,
/// nulla da committare), non per come l'agente ha scritto la chiamata — che ha
/// uno o due parametri, gia' validati dal contratto. Dire «rimediabile»
/// obbligherebbe il messaggio a spiegare COME, e qui non lo sappiamo: quello che
/// sappiamo lo dice git, e viaggia nel testo.
fn git_fallito(verbo: &str, e: impl std::fmt::Display) -> RispostaTool {
    RispostaTool::fallito_di_sistema(format!("[git {verbo} error: {e}]"))
}

pub async fn tool_git_status(ctx: &ToolContextCore) -> RispostaTool {
    if !ctx.is_git_repo {
        return non_e_un_repo();
    }
    match run_git_command(&ctx.root_path, &["status", "--porcelain=v1", "-b"]).await {
        // Un repository pulito e' una RISPOSTA, non un fallimento: e' lo stesso
        // criterio con cui `list_files` tratta una directory vuota.
        Ok((stdout, _)) => RispostaTool::riuscito(if stdout.trim().is_empty() {
            "Repository pulito, nessuna modifica pendente.".to_string()
        } else {
            stdout
        }),
        Err(e) => git_fallito("status", e),
    }
}

pub async fn tool_git_stage(ctx: &ToolContextCore, input: &Value) -> RispostaTool {
    use crate::{input_contract::InputTool, tool_inputs::GitStageInput};

    if !ctx.is_git_repo {
        return non_e_un_repo();
    }
    if let Some(negato) = permesso_di_scrittura(ctx) {
        return negato;
    }
    // La presenza e il TIPO di `paths` li pretende il contratto: la lettura a
    // mano scartava in silenzio gli elementi non-stringa (`filter_map`), quindi
    // `["a", 42]` metteva in staging il solo primo percorso e riportava
    // successo. Ora un array malformato e' un errore che nomina il campo.
    let params = match GitStageInput::leggi(input) {
        Ok(p) => p,
        Err(risposta) => return risposta,
    };
    if params.paths.is_empty() {
        return RispostaTool::fallito_rimediabile(
            "[Errore: 'paths' vuoto: indica almeno un percorso da mettere in staging]",
        );
    }
    let mut args = vec!["add"];
    args.extend(params.paths.iter().map(String::as_str));
    match run_git_command(&ctx.root_path, &args).await {
        Ok(_) => RispostaTool::riuscito(format!("Staged: {}", params.paths.join(", "))),
        Err(e) => git_fallito("add", e),
    }
}

pub async fn tool_git_commit(ctx: &ToolContextCore, input: &Value) -> RispostaTool {
    use crate::{input_contract::InputTool, tool_inputs::GitCommitInput};

    if !ctx.is_git_repo {
        return non_e_un_repo();
    }
    if let Some(negato) = permesso_di_scrittura(ctx) {
        return negato;
    }
    let params = match GitCommitInput::leggi(input) {
        Ok(p) => p,
        Err(risposta) => return risposta,
    };
    let message = params.message.as_str();
    match run_git_command(&ctx.root_path, &["commit", "-m", message]).await {
        Ok((stdout, _)) => {
            // Dispatcher: notifica GitStatusChanged → pannello Git aggiorna branch + counts
            if let Ok((status_out, _)) =
                run_git_command(&ctx.root_path, &["status", "--porcelain=v1", "-b"]).await
            {
                let branch = status_out
                    .lines()
                    .next()
                    .and_then(|l| l.strip_prefix("## "))
                    .map(|s| s.split("...").next().unwrap_or(s).to_string())
                    .unwrap_or_default();
                let modified_count = status_out
                    .lines()
                    .skip(1)
                    .filter(|l| !l.trim().is_empty())
                    .count();
                nexus_events::dispatcher::emit(
                    &ctx.project_channels,
                    ctx.project_id,
                    nexus_events::event::ProjectEvent::GitStatusChanged {
                        branch,
                        ahead: 0,
                        behind: 0,
                        modified_count: modified_count as i32,
                    },
                );
            }
            // Re-indicizza i file modificati nel commit in background
            // (contratto FileMutationHooks: l'impl mcp-core delega a reindex_single_file).
            let reindexer = ctx.hooks.clone();
            let project_id_bg = ctx.project_id;
            let root_bg = ctx.root_path.clone();
            tokio::spawn(async move {
                // Recupera i file dell'ultimo commit
                if let Ok((diff_out, _)) = run_git_command(
                    &root_bg,
                    &["diff-tree", "--no-commit-id", "-r", "--name-only", "HEAD"],
                )
                .await
                {
                    for line in diff_out.lines() {
                        let file_path = root_bg.join(line.trim());
                        if file_path.exists() {
                            reindexer
                                .reindex_file(project_id_bg, root_bg.clone(), file_path)
                                .await;
                        }
                    }
                }
            });
            RispostaTool::riuscito(stdout.trim())
        }
        Err(e) => git_fallito("commit", e),
    }
}

pub async fn tool_git_push(ctx: &ToolContextCore) -> RispostaTool {
    if !ctx.is_git_repo {
        return non_e_un_repo();
    }
    if let Some(negato) = permesso_di_scrittura(ctx) {
        return negato;
    }
    match run_git_command(&ctx.root_path, &["push"]).await {
        // git scrive il resoconto del push su STDERR anche quando riesce: il
        // ripiego non e' un errore mascherato, e' dove git mette l'output.
        Ok((stdout, stderr)) => RispostaTool::riuscito(
            if stdout.trim().is_empty() { stderr } else { stdout }.trim(),
        ),
        Err(e) => git_fallito("push", e),
    }
}

pub async fn tool_git_pull(ctx: &ToolContextCore) -> RispostaTool {
    if !ctx.is_git_repo {
        return non_e_un_repo();
    }
    match run_git_command(&ctx.root_path, &["pull", "--rebase"]).await {
        Ok((stdout, _)) => RispostaTool::riuscito(stdout.trim()),
        Err(e) => git_fallito("pull", e),
    }
}

/// Fix M16: configura un remote git (es. `origin`) puntando a un URL.
/// Tool agente che evita all'agente di usare `run_command git remote add ...` shell.
/// Input: `{name: string, url: string}` (default name = "origin")
pub async fn tool_git_remote_add(ctx: &ToolContextCore, input: &Value) -> RispostaTool {
    use crate::{input_contract::InputTool, tool_inputs::GitRemoteAddInput};

    if !ctx.is_git_repo {
        return non_e_un_repo();
    }
    if let Some(negato) = permesso_di_scrittura(ctx) {
        return negato;
    }
    let params = match GitRemoteAddInput::leggi(input) {
        Ok(p) => p,
        Err(risposta) => return risposta,
    };
    // Il default `origin` resta dell'handler: il contratto dichiara il campo
    // opzionale, non il valore che assume quando manca.
    let name = params.name.as_deref().unwrap_or("origin").trim();
    let url = params.url.trim();
    if url.is_empty() {
        return RispostaTool::fallito_rimediabile(
            "[Errore: 'url' vuoto: passa l'indirizzo del remote]",
        );
    }

    // Validazione: name puro alfanumerico/underscore/dash, no path traversal
    if !name
        .chars()
        .all(|c| c.is_alphanumeric() || c == '_' || c == '-')
    {
        // RIMEDIABILI entrambe: sono i due parametri della chiamata, e il
        // messaggio dice esattamente quale forma e' ammessa.
        return RispostaTool::fallito_rimediabile(format!(
            "[Errore: nome remote non valido '{name}' (solo alfanumerico/-/_)]"
        ));
    }

    // Validazione: url deve essere https:// o git@ (no file:// path locali per evitare leak)
    if !url.starts_with("https://") && !url.starts_with("git@") && !url.starts_with("ssh://") {
        return RispostaTool::fallito_rimediabile(format!(
            "[Errore: url remote deve iniziare con https://, git@ o ssh:// (rifiutato: '{url}')]"
        ));
    }

    // Se il remote esiste gia: rimuovilo e ricrealo (idempotente)
    let _ = run_git_command(&ctx.root_path, &["remote", "remove", name]).await;

    match run_git_command(&ctx.root_path, &["remote", "add", name, url]).await {
        Ok(_) => {
            // Verifica con git remote -v. Se la verifica non risponde il remote
            // e' comunque configurato: l'esito resta riuscito e il testo dice
            // solo cio' che si e' potuto accertare.
            match run_git_command(&ctx.root_path, &["remote", "-v"]).await {
                Ok((stdout, _)) => RispostaTool::riuscito(format!(
                    "Remote '{name}' configurato verso {url}.\n\nStato remote:\n{}",
                    stdout.trim()
                )),
                Err(_) => {
                    RispostaTool::riuscito(format!("Remote '{name}' configurato verso {url}."))
                }
            }
        }
        Err(e) => git_fallito("remote add", e),
    }
}
