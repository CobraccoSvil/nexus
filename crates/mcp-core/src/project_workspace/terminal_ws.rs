//! WebSocket del terminale IDE: `GET /api/neural/ws/terminal/:session_id`.
//!
//! Porting 1:1 dell'handler che viveva nel brain Python (`ws/terminal.py`),
//! ora morto. Apre una shell PTY confinata alla root del progetto, emessa e
//! firmata da `create_terminal_session` (project_workspace/workbench.rs).
//!
//! Autenticazione: NON Bearer. Il token firmato viaggia in query string
//! (`?token=payload.signature`) ed e' l'unico contratto, come per gli altri
//! `/api/neural/*` (vedi routes/neural_compat.rs). La firma e' ricalcolata col
//! punto unico `sign_terminal_token` (sha256(secret:payload) hex, regola L) e
//! confrontata a tempo costante.
//!
//! Async vs PTY sincrono: `portable-pty` e' un'API SINCRONA bloccante. Il
//! reader del master gira su un `std::thread` dedicato che inoltra i bytes su
//! un canale `tokio::mpsc`; il task async del WebSocket consuma il canale e
//! scrive sul socket. La scrittura WS->PTY usa il writer del master, anch'esso
//! mosso nel thread per non bloccare il runtime tokio.

use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

use axum::extract::ws::{Message, WebSocket, WebSocketUpgrade};
use axum::extract::{Path as AxumPath, Query, State};
use axum::http::StatusCode;
use axum::response::IntoResponse;
use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine as _};
use portable_pty::{native_pty_system, CommandBuilder, PtySize};
use serde::Deserialize;
use tokio::sync::mpsc;

use crate::projects::{sign_terminal_token, terminal_session_secret};
use crate::AppState;

/// Buffer ring server-side: 16 KB, identico al brain.
const OUTPUT_RING_BYTES: usize = 16 * 1024;
/// Tagli finali salvati in `terminal_commands.full_output`.
const DB_OUTPUT_MAX_CHARS: usize = 8000;
/// Chunk di lettura dal master PTY.
const PTY_READ_CHUNK: usize = 4096;

#[derive(Debug, Deserialize)]
pub struct TerminalWsQuery {
    pub token: Option<String>,
}

/// Claims del token firmato emesso da `create_terminal_session`. Replica i
/// campi di `TerminalSessionClaims` (workbench.rs) lato decodifica.
#[derive(Debug, Deserialize)]
struct TerminalClaims {
    sid: String,
    #[allow(dead_code)]
    uid: String,
    #[allow(dead_code)]
    pid: String,
    root: String,
    cwd: String,
    #[serde(default)]
    shell: Option<String>,
    exp: u64,
}

/// Messaggio di controllo che il frontend invia come testo JSON sul WS.
#[derive(Debug, Deserialize)]
struct ControlMessage {
    #[serde(rename = "type")]
    kind: String,
    #[serde(default)]
    rows: Option<u16>,
    #[serde(default)]
    cols: Option<u16>,
}

/// Upgrade HTTP->WebSocket. La verifica del token avviene PRIMA dell'upgrade:
/// se fallisce non si esegue l'handshake e si ritorna 403, cosi' il client non
/// resta appeso. Una volta accettato, eventuali errori di sessione chiudono il
/// socket col codice 4403 (parita' col brain).
pub async fn terminal_ws_upgrade(
    ws: WebSocketUpgrade,
    State(state): State<AppState>,
    AxumPath(session_id): AxumPath<String>,
    Query(query): Query<TerminalWsQuery>,
) -> impl IntoResponse {
    let secret = terminal_session_secret(&state.db).await;
    let claims = match verify_terminal_token(query.token.as_deref(), &session_id, &secret) {
        Ok(claims) => claims,
        Err(reason) => {
            tracing::warn!(
                session_id = %session_id,
                reason = %reason,
                "terminal_ws: token rifiutato"
            );
            return (StatusCode::FORBIDDEN, "[Terminal session non valida]").into_response();
        }
    };

    let db = state.db.clone();
    ws.on_upgrade(move |socket| async move {
        if let Err(err) = run_terminal_session(socket, db, session_id, claims).await {
            tracing::warn!(error = %err, "terminal_ws: sessione terminata con errore");
        }
    })
}

/// Errori di verifica del token. Volutamente non rivelati al client (solo log):
/// il messaggio inviato e' generico "[Terminal session non valida]".
#[derive(Debug, thiserror::Error)]
enum TokenError {
    #[error("token mancante o malformato")]
    Malformed,
    #[error("firma non valida")]
    BadSignature,
    #[error("payload non decodificabile")]
    BadPayload,
    #[error("sessione scaduta")]
    Expired,
    #[error("sid non corrispondente al path")]
    SidMismatch,
    #[error("cwd o root mancanti")]
    MissingPaths,
    #[error("cwd fuori dalla root del progetto")]
    CwdOutsideRoot,
}

/// Verifica il token del terminale.
///
/// Replica `_verify_terminal_token` (brain runtime.py) ma SENZA il controllo di
/// perimetro admin (allowed_roots / progetto registrato): `create_terminal_session`
/// ha gia validato `can_write` e impostato `cwd = root` del progetto. Qui si
/// verifica solo: firma, sid, scadenza, presenza di cwd/root e contenimento
/// `cwd` dentro `root` (canonicalizzato).
fn verify_terminal_token(
    token: Option<&str>,
    session_id: &str,
    secret: &str,
) -> Result<TerminalClaims, TokenError> {
    let token = token.unwrap_or("");
    let (payload_segment, signature) = token.split_once('.').ok_or(TokenError::Malformed)?;
    if payload_segment.is_empty() || signature.is_empty() {
        return Err(TokenError::Malformed);
    }

    // Confronto firma a tempo costante (punto unico sign_terminal_token, regola L).
    let expected = sign_terminal_token(payload_segment, secret);
    if !constant_time_eq(signature.as_bytes(), expected.as_bytes()) {
        return Err(TokenError::BadSignature);
    }

    let payload_bytes = URL_SAFE_NO_PAD
        .decode(payload_segment)
        .map_err(|_| TokenError::BadPayload)?;
    let claims: TerminalClaims =
        serde_json::from_slice(&payload_bytes).map_err(|_| TokenError::BadPayload)?;

    if claims.sid != session_id {
        return Err(TokenError::SidMismatch);
    }

    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(u64::MAX);
    if claims.exp <= now {
        return Err(TokenError::Expired);
    }

    if claims.cwd.trim().is_empty() || claims.root.trim().is_empty() {
        return Err(TokenError::MissingPaths);
    }

    // Contenimento cwd dentro root (canonicalizzato). Isolamento per-progetto
    // (regola E): la guard bash applica lo stesso vincolo a runtime, ma il
    // primo cwd deve gia stare nel perimetro.
    let resolved_root = canonicalize_lenient(&claims.root);
    let resolved_cwd = canonicalize_lenient(&claims.cwd);
    if !resolved_cwd.starts_with(&resolved_root) {
        return Err(TokenError::CwdOutsideRoot);
    }

    Ok(claims)
}

/// Canonicalizza un path se esiste sul filesystem, altrimenti normalizza
/// senza I/O. Evita falsi negativi quando `canonicalize` fallisce (es. symlink
/// gia risolti a monte da create_terminal_session).
///
/// Il risultato passa dal punto unico `path_for_storage` (regola L): su
/// Windows `canonicalize` produce la forma verbatim `\\?\D:\...`, e usarla
/// come cwd della shell fa mostrare a PowerShell il prompt provider-qualified
/// `PS Microsoft.PowerShell.Core\FileSystem::\\?\D:\...>` al posto del path.
fn canonicalize_lenient(raw: &str) -> PathBuf {
    let path = Path::new(raw);
    match std::fs::canonicalize(path) {
        Ok(canon) => PathBuf::from(nexus_types::workspace_paths::path_for_storage(&canon)),
        Err(_) => path.to_path_buf(),
    }
}

/// Confronto a tempo costante (no early-exit) per evitare timing attack sulla
/// firma. Stesso schema usato in browser-bridge-mcp.
fn constant_time_eq(a: &[u8], b: &[u8]) -> bool {
    if a.len() != b.len() {
        return false;
    }
    let mut diff = 0u8;
    for (x, y) in a.iter().zip(b.iter()) {
        diff |= x ^ y;
    }
    diff == 0
}

/// Comando shell + eventuale file rc temporaneo da rimuovere alla chiusura.
struct ShellPlan {
    program: String,
    args: Vec<String>,
    rc_path: Option<PathBuf>,
}

/// Costruisce il comando shell con la guard bash IDENTICA a
/// `_prepare_shell_command` (brain runtime.py): genera il file rc temporaneo
/// con `export NEXUS_TERMINAL_ROOT/CWD` + funzioni `__nexus_guarded_cd`/`cd`/
/// `pushd`/`popd`/`__nexus_enforce_root`, e lancia `bash --noprofile --rcfile
/// <rc> -i`. Per shell non-bash lancia la shell diretta (`--login` su bash di
/// default, niente guard se non e' bash).
fn prepare_shell_command(claims: &TerminalClaims) -> anyhow::Result<ShellPlan> {
    let shell = claims
        .shell
        .as_deref()
        .filter(|value| !value.trim().is_empty())
        .map(|value| value.to_string())
        .unwrap_or_else(default_shell);

    let shell_exe = Path::new(&shell)
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("")
        .to_ascii_lowercase();
    let is_bash = shell_exe.contains("bash");

    if !is_bash {
        // Shell non-bash: nessuna guard rc, lancia diretta.
        return Ok(ShellPlan {
            program: shell,
            args: Vec::new(),
            rc_path: None,
        });
    }

    let root = canonicalize_lenient(&claims.root);
    let initial_cwd = {
        let cwd = canonicalize_lenient(&claims.cwd);
        if cwd.as_os_str().is_empty() {
            root.clone()
        } else {
            cwd
        }
    };

    let rc_content = render_guard_rc(&root, &initial_cwd);

    // File rc temporaneo. tempfile garantisce nome univoco; lo persistiamo su
    // disco e ne teniamo il path per rimuoverlo alla chiusura.
    let mut rc_file = tempfile::Builder::new()
        .prefix("nexus-term-")
        .suffix(".bashrc")
        .tempfile()?;
    rc_file.write_all(rc_content.as_bytes())?;
    let (_, rc_path) = rc_file.keep()?;

    Ok(ShellPlan {
        program: shell,
        args: vec![
            "--noprofile".to_string(),
            "--rcfile".to_string(),
            rc_path.to_string_lossy().to_string(),
            "-i".to_string(),
        ],
        rc_path: Some(rc_path),
    })
}

/// Shell di default, parita' con `_default_shell` del brain. `TERMINAL_SHELL`
/// non e' un segreto/modello (regola G): e' una preferenza di ambiente come nel
/// brain originale, gia usata da `terminal_shell()` in projects/mod.rs.
fn default_shell() -> String {
    if cfg!(windows) {
        std::env::var("TERMINAL_SHELL").unwrap_or_else(|_| "powershell.exe".to_string())
    } else {
        std::env::var("TERMINAL_SHELL").unwrap_or_else(|_| "bash".to_string())
    }
}

/// Rende il contenuto del file rc guard bash, IDENTICO al brain. I path sono
/// serializzati come stringhe JSON (quoting sicuro) come faceva `json.dumps`.
fn render_guard_rc(root: &Path, initial_cwd: &Path) -> String {
    let root_json = serde_json::to_string(&root.to_string_lossy().to_string())
        .unwrap_or_else(|_| "\"\"".to_string());
    let cwd_json = serde_json::to_string(&initial_cwd.to_string_lossy().to_string())
        .unwrap_or_else(|_| "\"\"".to_string());

    format!(
        r#"# Auto-generated by Nexus terminal guard
export NEXUS_TERMINAL_ROOT={root_json}
export NEXUS_TERMINAL_CWD={cwd_json}
__nexus_is_within_root() {{
  local candidate="$1"
  case "$candidate" in
    "$NEXUS_TERMINAL_ROOT"|"$NEXUS_TERMINAL_ROOT"/*) return 0 ;;
    *) return 1 ;;
  esac
}}
__nexus_resolve_path() {{
  local raw="$1"
  if [[ -z "$raw" ]]; then
    raw="$NEXUS_TERMINAL_ROOT"
  fi
  if ! realpath -m -- "$raw" 2>/dev/null; then
    return 1
  fi
}}
__nexus_guarded_cd() {{
  local destination resolved
  destination="${{1:-$NEXUS_TERMINAL_ROOT}}"
  resolved="$(__nexus_resolve_path "$destination")" || {{
    echo "Percorso non valido: $destination"
    return 1
  }}
  if ! __nexus_is_within_root "$resolved"; then
    echo "Operazione negata: non puoi uscire da $NEXUS_TERMINAL_ROOT"
    return 1
  fi
  builtin cd -- "$resolved"
}}
cd() {{
  __nexus_guarded_cd "$@"
}}
pushd() {{
  if [[ "$#" -eq 0 ]]; then
    __nexus_guarded_cd "$NEXUS_TERMINAL_ROOT"
  else
    __nexus_guarded_cd "$1"
  fi
}}
popd() {{
  local before current
  before="$(pwd -P 2>/dev/null || pwd)"
  builtin popd "$@" >/dev/null || return 1
  current="$(pwd -P 2>/dev/null || pwd)"
  if ! __nexus_is_within_root "$current"; then
    echo "Operazione negata: non puoi uscire da $NEXUS_TERMINAL_ROOT"
    builtin cd -- "$before"
    return 1
  fi
  dirs -v
}}
__nexus_enforce_root() {{
  local current
  current="$(pwd -P 2>/dev/null || pwd)"
  if ! __nexus_is_within_root "$current"; then
    echo "Percorso fuori root rilevato, ritorno a $NEXUS_TERMINAL_ROOT"
    builtin cd -- "$NEXUS_TERMINAL_ROOT"
  fi
}}
PROMPT_COMMAND="__nexus_enforce_root${{PROMPT_COMMAND:+;$PROMPT_COMMAND}}"
builtin cd -- "$NEXUS_TERMINAL_CWD"
"#
    )
}

/// Eventi prodotti dal thread PTY verso il task async del WebSocket.
enum PtyEvent {
    /// Bytes di output dal master.
    Output(Vec<u8>),
    /// Il processo shell e' terminato (eventuale exit code numerico).
    Exit(Option<i32>),
}

/// Ciclo di vita della sessione terminale: apre il PTY, avvia il thread reader
/// + scrittore, e fa da ponte col WebSocket finche' uno dei due lati chiude.
async fn run_terminal_session(
    socket: WebSocket,
    db: sqlx::PgPool,
    session_id: String,
    claims: TerminalClaims,
) -> anyhow::Result<()> {
    let plan = prepare_shell_command(&claims)?;
    let cwd = canonicalize_lenient(&claims.cwd);

    // Apre il PTY: 24x80 finche' non arriva il primo resize, come il brain.
    let pty_system = native_pty_system();
    let pair = pty_system
        .openpty(PtySize {
            rows: 24,
            cols: 80,
            pixel_width: 0,
            pixel_height: 0,
        })
        .map_err(|e| anyhow::anyhow!("openpty fallita: {e}"))?;

    let mut cmd = CommandBuilder::new(&plan.program);
    for arg in &plan.args {
        cmd.arg(arg);
    }
    cmd.cwd(&cwd);
    // portable-pty NON eredita l'ambiente del processo (a differenza di
    // std::process::Command). Senza PATH l'exec della shell relativa "bash" non
    // viene risolto -> il processo esce subito (127) -> EOF immediato sul master
    // -> il terminale si chiude all'istante e il frontend entra in loop di
    // riconnessione. Replichiamo `os.environ.copy()` del brain ereditando
    // l'ambiente, poi forziamo TERM.
    for (key, value) in std::env::vars() {
        cmd.env(key, value);
    }
    cmd.env("TERM", "xterm-256color");

    let mut child = pair
        .slave
        .spawn_command(cmd)
        .map_err(|e| anyhow::anyhow!("spawn shell fallita: {e}"))?;
    // Il lato slave non serve piu' al processo padre: chiuderlo evita che il
    // reader resti appeso quando la shell esce (EOF sul master).
    drop(pair.slave);
    tracing::info!(
        session_id = %session_id,
        program = %plan.program,
        args = ?plan.args,
        cwd = %cwd.display(),
        "terminal_ws: shell PTY avviata"
    );

    let killer = child.clone_killer();
    let mut reader = pair
        .master
        .try_clone_reader()
        .map_err(|e| anyhow::anyhow!("clone reader PTY fallita: {e}"))?;
    let mut writer = pair
        .master
        .take_writer()
        .map_err(|e| anyhow::anyhow!("take writer PTY fallita: {e}"))?;
    // `master` resta vivo (serve per il resize) mosso nel handle condiviso.
    let master = Arc::new(parking_lot::Mutex::new(pair.master));

    // Canale PTY-thread -> task async. unbounded: l'output del PTY non deve mai
    // bloccare il thread reader (che e' sincrono e bloccante).
    let (pty_tx, mut pty_rx) = mpsc::unbounded_channel::<PtyEvent>();
    // Canale task async -> PTY-writer thread (bytes da scrivere sulla shell).
    let (input_tx, input_rx) = std::sync::mpsc::channel::<Vec<u8>>();

    // Thread reader del master: legge bytes finche' EOF, poi attende l'exit.
    let reader_tx = pty_tx.clone();
    let reader_handle = std::thread::Builder::new()
        .name("nexus-term-reader".to_string())
        .spawn(move || {
            let mut buf = [0u8; PTY_READ_CHUNK];
            loop {
                match reader.read(&mut buf) {
                    Ok(0) => break,
                    Ok(n) => {
                        if reader_tx.send(PtyEvent::Output(buf[..n].to_vec())).is_err() {
                            break;
                        }
                    }
                    Err(_) => break,
                }
            }
            let exit_code = child.wait().ok().map(|status| status.exit_code() as i32);
            let _ = reader_tx.send(PtyEvent::Exit(exit_code));
        })?;

    // Thread writer del master: consuma i bytes dal canale sincrono. Esce
    // quando il canale si chiude (input_tx droppato dal task async).
    let writer_handle = std::thread::Builder::new()
        .name("nexus-term-writer".to_string())
        .spawn(move || {
            while let Ok(bytes) = input_rx.recv() {
                if writer.write_all(&bytes).is_err() {
                    break;
                }
                let _ = writer.flush();
            }
        })?;

    let (mut ws_sink, mut ws_stream) = {
        use futures::StreamExt;
        socket.split()
    };

    // Buffer ring server-side (16 KB) condiviso tra il loop di output e il
    // flush al DB. Mutex sincrono (parking_lot): sezioni critiche brevissime.
    let ring = Arc::new(parking_lot::Mutex::new(OutputRing::new(OUTPUT_RING_BYTES)));
    let total_seen = Arc::new(AtomicUsize::new(0));

    // Task di flush periodico (debounce ~5s di output stabile -> flush al DB).
    let flush_db = db.clone();
    let flush_ring = ring.clone();
    let flush_total = total_seen.clone();
    let flush_session = session_id.clone();
    let flush_task = tokio::spawn(async move {
        let mut last_total = 0usize;
        let mut stable_ticks = 0u32;
        loop {
            tokio::time::sleep(std::time::Duration::from_secs(1)).await;
            let current = flush_total.load(Ordering::Relaxed);
            if current == last_total {
                if current > 0 {
                    stable_ticks += 1;
                    if stable_ticks >= 5 {
                        let snapshot = flush_ring.lock().snapshot();
                        flush_output_to_db(&flush_db, &flush_session, &snapshot, None).await;
                        stable_ticks = 0;
                    }
                }
            } else {
                last_total = current;
                stable_ticks = 0;
            }
        }
    });

    let mut final_exit: Option<i32> = None;

    // Ponte principale: instrada output PTY -> WS e input WS -> PTY.
    loop {
        tokio::select! {
            // Output dal PTY (o terminazione del processo).
            event = pty_rx.recv() => {
                match event {
                    Some(PtyEvent::Output(bytes)) => {
                        {
                            let mut guard = ring.lock();
                            guard.push(&bytes);
                        }
                        total_seen.fetch_add(bytes.len(), Ordering::Relaxed);
                        use futures::SinkExt;
                        if ws_sink.send(Message::Binary(bytes)).await.is_err() {
                            tracing::warn!(session_id = %session_id, "terminal_ws: invio output al WS fallito (client disconnesso)");
                            break;
                        }
                    }
                    Some(PtyEvent::Exit(code)) => {
                        final_exit = code;
                        let dbg_out: String = String::from_utf8_lossy(&ring.lock().snapshot())
                            .chars()
                            .take(400)
                            .collect();
                        tracing::warn!(
                            session_id = %session_id,
                            exit_code = ?code,
                            output = %dbg_out,
                            "terminal_ws: shell terminata (process_exit)"
                        );
                        use futures::SinkExt;
                        let payload = serde_json::json!({
                            "type": "process_exit",
                            "exitCode": code,
                        });
                        let _ = ws_sink
                            .send(Message::Text(payload.to_string()))
                            .await;
                        break;
                    }
                    None => {
                        tracing::warn!(session_id = %session_id, "terminal_ws: canale PTY chiuso (reader terminato, nessun Exit ricevuto)");
                        break;
                    }
                }
            }
            // Input dal WebSocket.
            incoming = {
                use futures::StreamExt;
                ws_stream.next()
            } => {
                match incoming {
                    Some(Ok(Message::Binary(data))) => {
                        if input_tx.send(data).is_err() {
                            break;
                        }
                    }
                    Some(Ok(Message::Text(text))) => {
                        if let Some(resize) = parse_resize(&text) {
                            let _ = master.lock().resize(PtySize {
                                rows: resize.0,
                                cols: resize.1,
                                pixel_width: 0,
                                pixel_height: 0,
                            });
                            continue;
                        }
                        if input_tx.send(text.into_bytes()).is_err() {
                            break;
                        }
                    }
                    Some(Ok(Message::Close(frame))) => {
                        tracing::warn!(session_id = %session_id, ?frame, "terminal_ws: WS chiuso dal client (Close frame)");
                        break;
                    }
                    None => {
                        tracing::warn!(session_id = %session_id, "terminal_ws: WS stream terminato (None)");
                        break;
                    }
                    Some(Ok(_)) => {}
                    Some(Err(e)) => {
                        tracing::warn!(session_id = %session_id, error = %e, "terminal_ws: errore lettura WS");
                        break;
                    }
                }
            }
        }
    }

    // ── Cleanup robusto ──────────────────────────────────────────────────────
    flush_task.abort();

    // Flush finale con exit code (se gia noto), altrimenti senza.
    let snapshot = ring.lock().snapshot();
    flush_output_to_db(&db, &session_id, &snapshot, final_exit).await;

    // Chiudi il canale verso il writer thread (lo fa uscire dal recv loop).
    drop(input_tx);

    // Termina la shell se ancora viva: chiude il master (drop) sblocca il
    // reader; killer assicura la terminazione anche se la shell ignora EOF.
    {
        let mut killer = killer;
        let _ = killer.kill();
    }
    drop(master);

    // Attendi la fine dei thread su un blocking pool per non bloccare il runtime.
    let _ = tokio::task::spawn_blocking(move || {
        let _ = reader_handle.join();
        let _ = writer_handle.join();
    })
    .await;

    // Rimuovi il file rc temporaneo.
    if let Some(rc_path) = plan.rc_path {
        let _ = std::fs::remove_file(rc_path);
    }

    Ok(())
}

/// Parsea un messaggio di controllo testuale; ritorna `(rows, cols)` se e' un
/// resize valido. Qualsiasi altro testo (anche JSON non-resize) NON e' un
/// resize: il chiamante lo inoltra come input alla shell, parita' col brain.
fn parse_resize(text: &str) -> Option<(u16, u16)> {
    if !text.starts_with('{') {
        return None;
    }
    let msg: ControlMessage = serde_json::from_str(text).ok()?;
    if msg.kind != "resize" {
        return None;
    }
    Some((msg.rows?, msg.cols?))
}

/// Buffer ring a byte: conserva al massimo `cap` byte dell'output piu' recente.
struct OutputRing {
    chunks: std::collections::VecDeque<Vec<u8>>,
    len: usize,
    cap: usize,
}

impl OutputRing {
    fn new(cap: usize) -> Self {
        Self {
            chunks: std::collections::VecDeque::new(),
            len: 0,
            cap,
        }
    }

    fn push(&mut self, bytes: &[u8]) {
        self.chunks.push_back(bytes.to_vec());
        self.len += bytes.len();
        while self.len > self.cap {
            match self.chunks.pop_front() {
                Some(removed) => self.len -= removed.len(),
                None => break,
            }
        }
    }

    fn snapshot(&self) -> Vec<u8> {
        let mut out = Vec::with_capacity(self.len);
        for chunk in &self.chunks {
            out.extend_from_slice(chunk);
        }
        out
    }
}

/// Scrive il buffer di output nel DB per l'ultimo comando ancora aperto della
/// sessione. Replica `_flush_output_to_db` del brain: strip ANSI, ultimi 8000
/// caratteri, `UPDATE terminal_commands SET full_output, exit_code, finished_at`.
async fn flush_output_to_db(
    db: &sqlx::PgPool,
    session_id: &str,
    raw_output: &[u8],
    exit_code: Option<i32>,
) {
    if raw_output.is_empty() && exit_code.is_none() {
        return;
    }
    let session_uuid = match uuid::Uuid::parse_str(session_id) {
        Ok(value) => value,
        Err(_) => return,
    };

    let decoded = String::from_utf8_lossy(raw_output);
    // Redazione dei segreti (punto unico, vedi agent_processes) PRIMA del
    // clipping: il taglio agli ultimi 8000 caratteri potrebbe spezzare una
    // connection string e lasciar passare la credenziale.
    let clean = crate::agent_processes::redact_secrets_for_persistence(&strip_ansi(&decoded));
    let clipped: String = {
        let chars: Vec<char> = clean.chars().collect();
        if chars.len() > DB_OUTPUT_MAX_CHARS {
            chars[chars.len() - DB_OUTPUT_MAX_CHARS..].iter().collect()
        } else {
            chars.into_iter().collect()
        }
    };

    let result = sqlx::query(
        "UPDATE terminal_commands \
         SET full_output = $1, exit_code = $2, finished_at = NOW() \
         WHERE id = ( \
             SELECT id FROM terminal_commands \
             WHERE session_id = $3 AND full_output IS NULL \
             ORDER BY created_at DESC LIMIT 1 \
         )",
    )
    .bind(&clipped)
    .bind(exit_code)
    .bind(session_uuid)
    .execute(db)
    .await;

    if let Err(err) = result {
        tracing::debug!(error = %err, "terminal_ws: flush output al DB fallito");
    }
}

/// Rimuove le sequenze ANSI/escape dal testo prima di salvarlo. Replica
/// `_strip_ansi` del brain (CSI, OSC, charset designation, `\r`), poi trim.
fn strip_ansi(input: &str) -> String {
    use once_cell::sync::Lazy;
    use regex::Regex;
    static CSI: Lazy<Regex> = Lazy::new(|| Regex::new(r"\x1B\[[0-9;]*[A-Za-z]").unwrap());
    static OSC: Lazy<Regex> = Lazy::new(|| Regex::new(r"\x1B\][^\x07]*\x07").unwrap());
    static CHARSET: Lazy<Regex> = Lazy::new(|| Regex::new(r"\x1B\([A-Z]").unwrap());

    let step1 = CSI.replace_all(input, "");
    let step2 = OSC.replace_all(&step1, "");
    let step3 = CHARSET.replace_all(&step2, "");
    step3.replace('\r', "").trim().to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn canonicalize_lenient_mai_verbatim() {
        // Regressione prompt terminale: su Windows `canonicalize` produce la
        // forma verbatim (\\?\D:\...) e PowerShell avviato con quella cwd
        // mostra il prompt provider-qualified. Il percorso reale (una dir
        // esistente canonicalizzata) non deve MAI uscire in forma verbatim.
        let dir = std::env::temp_dir();
        let out = canonicalize_lenient(&dir.to_string_lossy());
        assert!(
            !out.to_string_lossy().starts_with(r"\\?\"),
            "cwd verbatim: {out:?}"
        );
        // Path inesistente: no-op senza I/O, invariato.
        let missing = canonicalize_lenient("relative/inesistente");
        assert_eq!(missing, PathBuf::from("relative/inesistente"));
    }

    fn make_token(secret: &str, claims_json: &serde_json::Value) -> String {
        let payload = URL_SAFE_NO_PAD.encode(serde_json::to_vec(claims_json).unwrap());
        let sig = sign_terminal_token(&payload, secret);
        format!("{payload}.{sig}")
    }

    #[test]
    fn verify_token_ok() {
        let secret = "test-secret";
        let root = std::env::temp_dir();
        let claims = serde_json::json!({
            "sid": "abc",
            "uid": "u1",
            "pid": "p1",
            "root": root.to_string_lossy(),
            "cwd": root.to_string_lossy(),
            "shell": "bash",
            "exp": u64::MAX,
        });
        let token = make_token(secret, &claims);
        let parsed = verify_terminal_token(Some(&token), "abc", secret).unwrap();
        assert_eq!(parsed.sid, "abc");
    }

    #[test]
    fn verify_token_rejects_bad_signature() {
        let secret = "test-secret";
        let claims = serde_json::json!({
            "sid": "abc", "uid": "u1", "pid": "p1",
            "root": "/tmp", "cwd": "/tmp", "shell": "bash", "exp": u64::MAX,
        });
        let mut token = make_token(secret, &claims);
        token.push('x');
        assert!(verify_terminal_token(Some(&token), "abc", secret).is_err());
    }

    #[test]
    fn verify_token_rejects_sid_mismatch() {
        let secret = "test-secret";
        let claims = serde_json::json!({
            "sid": "abc", "uid": "u1", "pid": "p1",
            "root": "/tmp", "cwd": "/tmp", "shell": "bash", "exp": u64::MAX,
        });
        let token = make_token(secret, &claims);
        assert!(verify_terminal_token(Some(&token), "different", secret).is_err());
    }

    #[test]
    fn verify_token_rejects_expired() {
        let secret = "test-secret";
        let claims = serde_json::json!({
            "sid": "abc", "uid": "u1", "pid": "p1",
            "root": "/tmp", "cwd": "/tmp", "shell": "bash", "exp": 1u64,
        });
        let token = make_token(secret, &claims);
        assert!(verify_terminal_token(Some(&token), "abc", secret).is_err());
    }

    #[test]
    fn verify_token_rejects_cwd_outside_root() {
        let secret = "test-secret";
        let claims = serde_json::json!({
            "sid": "abc", "uid": "u1", "pid": "p1",
            "root": "/tmp/nexus-root-xyz", "cwd": "/etc", "shell": "bash", "exp": u64::MAX,
        });
        let token = make_token(secret, &claims);
        assert!(verify_terminal_token(Some(&token), "abc", secret).is_err());
    }

    #[test]
    fn parse_resize_valid() {
        let text = r#"{"type":"resize","rows":40,"cols":120}"#;
        assert_eq!(parse_resize(text), Some((40, 120)));
    }

    #[test]
    fn parse_resize_non_resize_is_none() {
        assert_eq!(parse_resize(r#"{"type":"other"}"#), None);
        assert_eq!(parse_resize("plain text"), None);
    }

    #[test]
    fn output_ring_caps_to_capacity() {
        let mut ring = OutputRing::new(8);
        ring.push(b"12345");
        ring.push(b"67890");
        let snap = ring.snapshot();
        assert!(snap.len() <= 8);
        assert_eq!(snap, b"67890".to_vec().as_slice().to_vec());
    }

    #[test]
    fn strip_ansi_removes_escape_sequences() {
        let input = "\x1B[31mrosso\x1B[0m\r\nfine";
        let out = strip_ansi(input);
        assert_eq!(out, "rosso\nfine");
    }
}
