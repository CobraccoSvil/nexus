//! Coerenza della modalita' di avvio di un progetto: NATIVO vs CONTAINER.
//!
//! Punto unico (regola L) che classifica un servizio e permette di impedire che
//! un progetto abbia unit di avvio CONFLIGGENTI. Incidente reale (beauty-book):
//! coesistevano `backend.service` (npm run dev) + `frontend.service` (vite) E
//! `docker-compose.service` (docker compose up --build, che builda gli stessi
//! backend+frontend container). Tutti con Restart=on-failure: giravano insieme e
//! si mandavano SIGTERM a vicenda -> il sito non partiva. Causa radice: Nexus non
//! aveva il concetto di "modalita' di avvio del progetto", quindi wizard_install
//! creava unit nativi e container per lo stesso ruolo senza alcun gate.

/// Modalita' di avvio di un servizio (e, per estensione, del progetto).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RunMode {
    /// Processo nativo sull'host (npm/pnpm/dotnet/cargo/python/server statico).
    Native,
    /// Container (docker/podman compose o run).
    Container,
}

impl RunMode {
    /// Etichetta leggibile per i messaggi all'utente.
    pub fn label(self) -> &'static str {
        match self {
            RunMode::Native => "nativo",
            RunMode::Container => "container",
        }
    }
}

/// Classifica la modalita' a partire dall'`ExecStart`/command di un servizio.
/// Container se invoca docker/podman (compose o run); altrimenti Native.
pub fn run_mode_of(exec_start: &str) -> RunMode {
    let l = exec_start.to_lowercase();
    if l.contains("docker compose")
        || l.contains("docker-compose")
        || l.contains("docker run")
        || l.contains("podman compose")
        || l.contains("podman run")
    {
        RunMode::Container
    } else {
        RunMode::Native
    }
}

/// Estrae il valore della prima riga `ExecStart=` da un unit file systemd.
/// Ritorna None se l'unit non ha un ExecStart (es. file malformato).
pub fn exec_start_of_unit(unit_content: &str) -> Option<String> {
    unit_content.lines().find_map(|line| {
        line.trim_start()
            .strip_prefix("ExecStart=")
            .map(|s| s.trim().to_string())
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn container_da_docker() {
        assert_eq!(
            run_mode_of("docker compose -f a.yml -f b.yml up --build"),
            RunMode::Container
        );
        assert_eq!(
            run_mode_of("/usr/bin/docker-compose up"),
            RunMode::Container
        );
        assert_eq!(
            run_mode_of("docker run --rm app:latest"),
            RunMode::Container
        );
        assert_eq!(run_mode_of("podman compose up"), RunMode::Container);
    }

    #[test]
    fn native_da_npm_vite_dotnet_python() {
        assert_eq!(run_mode_of("/usr/bin/npm run dev"), RunMode::Native);
        assert_eq!(
            run_mode_of("npx vite --host 0.0.0.0 --port 39550"),
            RunMode::Native
        );
        assert_eq!(
            run_mode_of("dotnet run --project x.csproj"),
            RunMode::Native
        );
        assert_eq!(run_mode_of("python3 -m http.server 39560"), RunMode::Native);
        assert_eq!(run_mode_of("cargo run --bin server"), RunMode::Native);
    }

    #[test]
    fn estrae_exec_start_dall_unit() {
        let unit = "[Unit]\nDescription=x\n\n[Service]\nWorkingDirectory=/p\nExecStart=/usr/bin/npm run dev\nRestart=on-failure\n";
        assert_eq!(
            exec_start_of_unit(unit).as_deref(),
            Some("/usr/bin/npm run dev")
        );
        assert_eq!(exec_start_of_unit("nessun exec qui"), None);
    }

    #[test]
    fn conflitto_nativo_vs_container() {
        // Un backend npm (nativo) e un docker-compose (container) collidono.
        assert_ne!(
            run_mode_of("/usr/bin/npm run dev"),
            run_mode_of("docker compose -f docker-compose.yml up --build")
        );
    }
}
