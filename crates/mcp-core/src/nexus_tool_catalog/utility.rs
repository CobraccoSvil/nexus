//! Registrazione handler dominio: utility
//!
//! Generato dal refactor di `nexus_tool_catalog.rs` (god-file split).
//! Nessun cambiamento di comportamento: spostamento puro delle
//! chiamate `register_with_handler` raggruppate per dominio.

use super::{NexusToolCatalog, NexusToolCategory, NexusToolSpec};
use std::sync::Arc;

pub(super) fn register(c: &NexusToolCatalog) {
    use crate::nexus_tools::{
        base64_decode::Base64DecodeTool, base64_encode::Base64EncodeTool, env_get::EnvGetTool,
        fs_glob::FsGlobTool, fs_grep::FsGrepTool, fs_list::FsListTool, fs_read::FsReadTool,
        fs_stat::FsStatTool, fs_tree::FsTreeTool, fs_write::FsWriteTool,
        hash_content::HashContentTool, http_request::HttpRequestTool, json_get::JsonGetTool,
        json_parse::JsonParseTool, project_delete::ProjectDeleteTool,
        project_info::ProjectInfoTool,
        project_register_existing_dir::ProjectRegisterExistingDirTool,
        project_register_from_git::ProjectRegisterFromGitTool,
        project_run_configs::ProjectRunConfigsTool,
        project_set_default_branch::ProjectSetDefaultBranchTool,
        project_workspace_init::ProjectWorkspaceInitTool, regex_match::RegexMatchTool,
        regex_replace::RegexReplaceTool, service_healthcheck::ServiceHealthcheckTool,
        text_diff::TextDiffTool, time_now::TimeNowTool, uuid_generate::UuidGenerateTool,
        uuid_parse::UuidParseTool,
    };

    // Utility (Fase 9C)
    c.register_with_handler(
        NexusToolSpec::new(
            "regex_match",
            NexusToolCategory::Utility,
            "Run a regex over inline text or a project file and return matches",
        ),
        Arc::new(RegexMatchTool),
    );

    // HTTP + Healthcheck tools
    c.register_with_handler(
        NexusToolSpec::new(
            "http_request",
            NexusToolCategory::Utility,
            "Esegue una richiesta HTTP strutturata (GET/POST/PUT/PATCH/DELETE/HEAD/OPTIONS). Restituisce status, headers, body (JSON o testo), latenza. Ideale per testare endpoint del progetto.",
        ),
        Arc::new(HttpRequestTool),
    );
    c.register_with_handler(
        NexusToolSpec::new(
            "service_healthcheck",
            NexusToolCategory::Utility,
            "Verifica che un servizio sia raggiungibile tramite probe HTTP o TCP (tcp://host:port). Retry con backoff esponenziale. Restituisce ok, status, latenza.",
        ),
        Arc::new(ServiceHealthcheckTool),
    );

    // Fase 4: Bootstrap progetto
    c.register_with_handler(
        NexusToolSpec::new(
            "project_register_from_git",
            NexusToolCategory::Utility,
            "Clona un repository Git e lo registra come progetto Nexus. Esegue git clone --depth=1, inserisce in projects/workspaces/repositories con transazione atomica.",
        ),
        Arc::new(ProjectRegisterFromGitTool),
    );
    c.register_with_handler(
        NexusToolSpec::new(
            "project_register_existing_dir",
            NexusToolCategory::Utility,
            "Registra una directory gia' presente sul filesystem come progetto Nexus. Non esegue clone, rileva info Git, inserisce in DB con transazione.",
        ),
        Arc::new(ProjectRegisterExistingDirTool),
    );
    c.register_with_handler(
        NexusToolSpec::new(
            "project_delete",
            NexusToolCategory::Utility,
            "Soft-delete di un progetto dal DB. Rimuove righe da projects e tabelle dipendenti (CASCADE). Non cancella file dal disco. Richiede confirm:true.",
        ),
        Arc::new(ProjectDeleteTool),
    );
    c.register_with_handler(
        NexusToolSpec::new(
            "project_set_default_branch",
            NexusToolCategory::Utility,
            "Aggiorna il branch predefinito di un progetto (es. da develop a main).",
        ),
        Arc::new(ProjectSetDefaultBranchTool),
    );
    c.register_with_handler(
        NexusToolSpec::new(
            "project_workspace_init",
            NexusToolCategory::Utility,
            "Inizializza la riga workspaces per un progetto. Utile dopo clone manuale o registrazione incompleta. Idempotente: se il workspace esiste gia', ritorna l'ID esistente.",
        ),
        Arc::new(ProjectWorkspaceInitTool),
    );

    // Project config tools — info progetto, run configs
    c.register_with_handler(
        NexusToolSpec::new(
            "project_info",
            NexusToolCategory::Utility,
            "Info generali del progetto: nome, root path, git branch, stack rilevato, istruzioni custom, sandbox config.",
        ),
        Arc::new(ProjectInfoTool),
    );
    c.register_with_handler(
        NexusToolSpec::new(
            "project_run_configs",
            NexusToolCategory::Utility,
            "Configurazioni di esecuzione (comandi) disponibili per il progetto: label, tipo, comando, args, cwd, env.",
        ),
        Arc::new(ProjectRunConfigsTool),
    );

    // Fase 9F: Utility batch (10)
    c.register_with_handler(
        NexusToolSpec::new(
            "fs_read",
            NexusToolCategory::Utility,
            "Read a file from the project with optional line range",
        ),
        Arc::new(FsReadTool),
    );
    c.register_with_handler(
        NexusToolSpec::new(
            "fs_list",
            NexusToolCategory::Utility,
            "List files in a project directory with regex filter",
        ),
        Arc::new(FsListTool),
    );
    c.register_with_handler(
        NexusToolSpec::new(
            "fs_grep",
            NexusToolCategory::Utility,
            "Recursive regex search across project files",
        ),
        Arc::new(FsGrepTool),
    );
    c.register_with_handler(
        NexusToolSpec::new(
            "fs_tree",
            NexusToolCategory::Utility,
            "Project file tree as JSON",
        ),
        Arc::new(FsTreeTool),
    );
    c.register_with_handler(
        NexusToolSpec::new(
            "json_parse",
            NexusToolCategory::Utility,
            "Parse and pretty-print JSON (inline or from file)",
        ),
        Arc::new(JsonParseTool),
    );
    c.register_with_handler(
        NexusToolSpec::new(
            "json_get",
            NexusToolCategory::Utility,
            "Extract a value from JSON via dot-path query",
        ),
        Arc::new(JsonGetTool),
    );
    c.register_with_handler(
        NexusToolSpec::new(
            "base64_encode",
            NexusToolCategory::Utility,
            "Base64 encode (standard or url-safe)",
        ),
        Arc::new(Base64EncodeTool),
    );
    c.register_with_handler(
        NexusToolSpec::new(
            "base64_decode",
            NexusToolCategory::Utility,
            "Base64 decode to UTF-8 string",
        ),
        Arc::new(Base64DecodeTool),
    );
    c.register_with_handler(
        NexusToolSpec::new(
            "hash_content",
            NexusToolCategory::Utility,
            "SHA-256/SHA-512 hash of a string or file",
        ),
        Arc::new(HashContentTool),
    );
    c.register_with_handler(
        NexusToolSpec::new(
            "uuid_generate",
            NexusToolCategory::Utility,
            "Generate UUID v4 (batch, optional compact form)",
        ),
        Arc::new(UuidGenerateTool),
    );

    // Fase 9G: Utility batch (8)
    c.register_with_handler(
        NexusToolSpec::new(
            "fs_write",
            NexusToolCategory::Utility,
            "Write text to a file inside project_root (overwrite or append)",
        ),
        Arc::new(FsWriteTool),
    );
    c.register_with_handler(
        NexusToolSpec::new(
            "fs_stat",
            NexusToolCategory::Utility,
            "File/dir metadata (size, mtime, type, readonly)",
        ),
        Arc::new(FsStatTool),
    );
    c.register_with_handler(
        NexusToolSpec::new(
            "fs_glob",
            NexusToolCategory::Utility,
            "Glob match (`*`, `?`) recursive over project files",
        ),
        Arc::new(FsGlobTool),
    );
    c.register_with_handler(
        NexusToolSpec::new(
            "env_get",
            NexusToolCategory::Utility,
            "Read environment variables (with secret masking by default)",
        ),
        Arc::new(EnvGetTool),
    );
    c.register_with_handler(
        NexusToolSpec::new(
            "time_now",
            NexusToolCategory::Utility,
            "Current UTC timestamp in unix/iso8601/rfc3339 formats",
        ),
        Arc::new(TimeNowTool),
    );
    c.register_with_handler(
        NexusToolSpec::new(
            "regex_replace",
            NexusToolCategory::Utility,
            "Regex replace on a string or file content (read-only, in-memory)",
        ),
        Arc::new(RegexReplaceTool),
    );
    c.register_with_handler(
        NexusToolSpec::new(
            "text_diff",
            NexusToolCategory::Utility,
            "Line-based LCS diff between two texts or files",
        ),
        Arc::new(TextDiffTool),
    );
    c.register_with_handler(
        NexusToolSpec::new(
            "uuid_parse",
            NexusToolCategory::Utility,
            "Validate and describe a UUID string (version, variant)",
        ),
        Arc::new(UuidParseTool),
    );
}
