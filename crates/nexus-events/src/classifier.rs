//! Classifier: regole hardcoded che mappano `ProjectEvent -> UiHint`.
//!
//! Copre il 90% dei casi noti senza chiamare LLM.
//! Eventi non coperti restituiscono `None` (il chiamante puo' decidere di
//! delegare a LLM fallback).
//!
//! Lingua dei `toast_msg`: italiano (utente italiano, regola CLAUDE.md).

use crate::event::{ProjectEvent, UiHint};

#[derive(Debug, Clone, Default)]
pub struct Classifier {
    // Slot per LLM fallback future. Per ora: solo regole.
}

impl Classifier {
    pub fn rules_only() -> Self {
        Self::default()
    }

    pub fn classify(&self, ev: &ProjectEvent) -> Option<UiHint> {
        match ev {
            // ── Playwright job ─────────────────────────────────────────
            ProjectEvent::JobCreated {
                job_kind, status, ..
            } if job_kind == "playwright_test" => {
                let (sev, msg, badge) = match status.as_str() {
                    "passed" => (
                        Some("success".into()),
                        Some("Test Playwright completati".into()),
                        None,
                    ),
                    "failed" => (
                        Some("error".into()),
                        Some("Test Playwright falliti".into()),
                        Some(("playwright".into(), 1)),
                    ),
                    _ => (None, None, None),
                };
                Some(UiHint {
                    highlight_panel: Some("playwright".into()),
                    toast_severity: sev,
                    toast_msg: msg,
                    badge_increment: badge,
                    flash_duration_ms: Some(800),
                })
            }
            // Per JobCreated non-playwright (es. test generici): nessun hint
            ProjectEvent::JobCreated { .. } => None,
            ProjectEvent::JobUpdated { status, .. } if status == "failed" => Some(UiHint {
                highlight_panel: Some("playwright".into()),
                toast_severity: Some("error".into()),
                toast_msg: Some("Esecuzione test fallita".into()),
                flash_duration_ms: Some(800),
                ..Default::default()
            }),
            ProjectEvent::JobUpdated { .. } => None, // job non-failed: nessun hint
            ProjectEvent::JobsCleared { deleted, .. } => Some(UiHint {
                toast_severity: Some("info".into()),
                toast_msg: Some(format!("Cronologia run pulita ({} eliminati)", deleted)),
                ..Default::default()
            }),

            // ── Ports ──────────────────────────────────────────────────
            ProjectEvent::PortAllocated { port, label, .. } => Some(UiHint {
                highlight_panel: Some("ports".into()),
                toast_severity: Some("info".into()),
                toast_msg: Some(format!("Porta {} allocata ({})", port, label)),
                flash_duration_ms: Some(500),
                ..Default::default()
            }),
            ProjectEvent::PortReleased { port } => Some(UiHint {
                highlight_panel: Some("ports".into()),
                toast_severity: Some("info".into()),
                toast_msg: Some(format!("Porta {} rilasciata", port)),
                ..Default::default()
            }),

            // ── Problems / quality ─────────────────────────────────────
            ProjectEvent::FindingsUpdated {
                total, critical, ..
            } => Some(UiHint {
                highlight_panel: if *critical > 0 {
                    Some("problems".into())
                } else {
                    None
                },
                toast_severity: if *critical > 0 {
                    Some("warning".into())
                } else {
                    None
                },
                toast_msg: if *critical > 0 {
                    Some(format!("{} finding critici", critical))
                } else {
                    None
                },
                badge_increment: Some(("problems".into(), *total as i32)),
                ..Default::default()
            }),

            // ── Services ───────────────────────────────────────────────
            ProjectEvent::ServiceStarted { name, port, .. } => Some(UiHint {
                highlight_panel: Some("services".into()),
                toast_severity: Some("success".into()),
                toast_msg: Some(match port {
                    Some(p) => format!("Servizio {} avviato (porta {})", name, p),
                    None => format!("Servizio {} avviato", name),
                }),
                ..Default::default()
            }),
            ProjectEvent::ServiceStopped { name } => Some(UiHint {
                highlight_panel: Some("services".into()),
                toast_severity: Some("info".into()),
                toast_msg: Some(format!("Servizio {} fermato", name)),
                ..Default::default()
            }),
            ProjectEvent::ServiceRestarted { name } => Some(UiHint {
                highlight_panel: Some("services".into()),
                toast_severity: Some("info".into()),
                toast_msg: Some(format!("Servizio {} riavviato", name)),
                ..Default::default()
            }),

            // ── Filesystem / git ──────────────────────────────────────
            ProjectEvent::FileChanged { .. } => Some(UiHint {
                // Niente toast (rumoroso), solo flash sull'Explorer
                flash_duration_ms: Some(200),
                ..Default::default()
            }),
            ProjectEvent::GitStatusChanged {
                modified_count, ..
            } => Some(UiHint {
                badge_increment: Some(("git".into(), *modified_count)),
                ..Default::default()
            }),

            // ── Database ───────────────────────────────────────────────
            ProjectEvent::DbQueryRun { duration_ms, .. } if *duration_ms > 1000 => {
                Some(UiHint {
                    highlight_panel: Some("database".into()),
                    toast_severity: Some("warning".into()),
                    toast_msg: Some(format!("Query lenta: {}ms", duration_ms)),
                    ..Default::default()
                })
            }
            ProjectEvent::DbQueryRun { .. } => None, // query veloci non emettono hint
            ProjectEvent::DbConfigUpdated { name, action, .. } => Some(UiHint {
                highlight_panel: Some("database".into()),
                toast_severity: Some("success".into()),
                toast_msg: Some(format!(
                    "Connessione DB '{}' {}",
                    name,
                    match action.as_str() {
                        "created" => "configurata",
                        "updated" => "aggiornata",
                        "deleted" => "rimossa",
                        _ => "modificata",
                    }
                )),
                flash_duration_ms: Some(600),
                ..Default::default()
            }),

            // ── Notifiche dall'agente (gia' decise dal modello) ───────
            ProjectEvent::Notification {
                severity,
                message,
                panel,
                ..
            } => Some(UiHint {
                highlight_panel: panel.clone(),
                toast_severity: Some(severity.clone()),
                toast_msg: Some(message.clone()),
                ..Default::default()
            }),
            ProjectEvent::HighlightPanel {
                panel,
                duration_ms,
            } => Some(UiHint {
                highlight_panel: Some(panel.clone()),
                flash_duration_ms: Some(*duration_ms),
                ..Default::default()
            }),
            ProjectEvent::FlagChanged { .. } | ProjectEvent::MonitorUpdated { .. } => None,

            // ── Agent meta + custom + system ──────────────────────────
            ProjectEvent::AgentToolUsed { .. } => None,
            ProjectEvent::Custom { .. } => None, // candidato a LLM fallback futuro
            ProjectEvent::SnapshotRequired { .. } => None,

            // ── Chat sessions ─────────────────────────────────────────
            // Compatta: toast info breve + highlight tab chat. La barra
            // token si aggiorna direttamente dal payload, qui solo feedback.
            ProjectEvent::ChatSessionCompacted { total_tokens, .. } => Some(UiHint {
                highlight_panel: Some("chat".into()),
                toast_severity: Some("info".into()),
                toast_msg: Some(format!(
                    "Chat compattata ({} token in memoria)",
                    total_tokens
                )),
                flash_duration_ms: Some(400),
                ..Default::default()
            }),
            // Messaggio aggiunto: silente (sarebbe rumoroso un toast per ogni
            // messaggio). Il binding use-chat aggiorna tokenUsage senza UI hint.
            ProjectEvent::ChatMessageAdded { .. } => None,
            // Cambio stato sessione: silente (i tab gia' mostrano l'icona).
            ProjectEvent::ChatSessionStatusChanged { .. } => None,

            // ── Catch-all mutazioni HTTP ──────────────────────────────
            // Silente per default (sarebbe troppo rumoroso un toast per ogni
            // mutazione HTTP). I pannelli interessati ascoltano via
            // `useEventOfKind("mutation_recorded", ...)` se serve.
            ProjectEvent::MutationRecorded { .. } => None,

            // ── Project lifecycle ─────────────────────────────────────
            ProjectEvent::ProjectCreated { name, .. } => Some(UiHint {
                toast_severity: Some("success".into()),
                toast_msg: Some(format!("Progetto '{}' creato", name)),
                flash_duration_ms: Some(600),
                ..Default::default()
            }),
            ProjectEvent::ProjectDeleted { name } => Some(UiHint {
                toast_severity: Some("info".into()),
                toast_msg: Some(format!("Progetto '{}' eliminato", name)),
                flash_duration_ms: Some(600),
                ..Default::default()
            }),

            // ── Database migrations ──────────────────────────────────
            ProjectEvent::MigrationApplied { migration_name, version } => Some(UiHint {
                highlight_panel: Some("database".into()),
                toast_severity: Some("success".into()),
                toast_msg: Some(format!("Migrazione '{}' (v{}) applicata", migration_name, version)),
                flash_duration_ms: Some(600),
                ..Default::default()
            }),
            ProjectEvent::MigrationRolledBack { migration_name, version } => Some(UiHint {
                highlight_panel: Some("database".into()),
                toast_severity: Some("warning".into()),
                toast_msg: Some(format!("Migrazione '{}' (v{}) annullata", migration_name, version)),
                flash_duration_ms: Some(600),
                ..Default::default()
            }),

            // ── Run configurations ───────────────────────────────────
            ProjectEvent::RunConfigChanged { label, action, .. } => Some(UiHint {
                highlight_panel: Some("services".into()),
                toast_severity: Some("info".into()),
                toast_msg: Some(format!(
                    "Config run '{}' {}",
                    label,
                    match action.as_str() {
                        "created" => "creata",
                        "updated" => "aggiornata",
                        "deleted" => "eliminata",
                        _ => "modificata",
                    }
                )),
                flash_duration_ms: Some(400),
                ..Default::default()
            }),

            // ── Memory ───────────────────────────────────────────────
            ProjectEvent::MemoryUpdated { category, count_delta } => {
                let action = if *count_delta > 0 { "aggiunto" } else { "rimosso" };
                Some(UiHint {
                    toast_severity: Some("info".into()),
                    toast_msg: Some(format!("Memoria {}: elemento {}", category, action)),
                    flash_duration_ms: Some(300),
                    ..Default::default()
                })
            }

            // ── Provider health ───────────────────────────────────────
            ProjectEvent::ProviderHealthChanged { provider, status, .. } => {
                let (sev, msg) = match status.as_str() {
                    "down" => ("error", format!("Provider {} non raggiungibile", provider)),
                    "degraded" => ("warning", format!("Provider {} degradato", provider)),
                    _ => ("success", format!("Provider {} operativo", provider)),
                };
                Some(UiHint {
                    toast_severity: Some(sev.into()),
                    toast_msg: Some(msg),
                    flash_duration_ms: Some(400),
                    ..Default::default()
                })
            }

            // ── Plugin lifecycle ─────────────────────────────────────
            ProjectEvent::PluginChanged { slug, action, .. } => Some(UiHint {
                toast_severity: Some("info".into()),
                toast_msg: Some(format!(
                    "Plugin '{}' {}",
                    slug,
                    match action.as_str() {
                        "installed" => "installato",
                        "uninstalled" => "disinstallato",
                        "enabled" => "abilitato",
                        "disabled" => "disabilitato",
                        _ => "modificato",
                    }
                )),
                flash_duration_ms: Some(400),
                ..Default::default()
            }),

            // ── Settings ─────────────────────────────────────────────
            ProjectEvent::SettingChanged { .. } => None, // silente, i pannelli reagiscono
            // ── Subagent runs ────────────────────────────────────────
            ProjectEvent::SubagentRunChanged { .. } => None, // silente, pannello orchestratore reagisce
            // ── Quality scan progress ────────────────────────────────
            ProjectEvent::QualityScanProgress { phase, percent, .. } => {
                if phase == "started" {
                    Some(UiHint {
                        highlight_panel: Some("problems".into()),
                        toast_severity: Some("info".into()),
                        toast_msg: Some("Scansione qualita' avviata".into()),
                        flash_duration_ms: Some(300),
                        ..Default::default()
                    })
                } else if phase == "completed" {
                    Some(UiHint {
                        highlight_panel: Some("problems".into()),
                        toast_severity: Some("success".into()),
                        toast_msg: Some("Scansione qualita' completata".into()),
                        flash_duration_ms: Some(600),
                        ..Default::default()
                    })
                } else {
                    // progress: silente, aggiorna solo la barra
                    let _ = percent; // evita warning unused
                    None
                }
            }
            // ── Output channels ──────────────────────────────────────
            ProjectEvent::OutputChannelCreated { .. } => None, // silente

            // ── Eventi di arricchimento ───────────────────────────────
            // EventEnriched e' il VEICOLO di hint, non un evento da
            // arricchire ulteriormente. Ritorna None per evitare loop.
            // ── Knowledge Base ────────────────────────────────────
            ProjectEvent::KnowledgeNoteCreated { title, .. } => Some(UiHint {
                highlight_panel: Some("knowledge".to_string()),
                toast_severity: Some("info".to_string()),
                toast_msg: Some(format!("Nota KB creata: {title}")),
                badge_increment: None,
                flash_duration_ms: Some(2000),
            }),
            ProjectEvent::KnowledgeNoteUpdated { .. } => None, // silente
            ProjectEvent::KnowledgeLinkCreated { .. } => None,  // silente

            ProjectEvent::EventEnriched { .. } => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use uuid::Uuid;

    #[test]
    fn playwright_failed_triggers_error_toast() {
        let c = Classifier::rules_only();
        let hint = c
            .classify(&ProjectEvent::JobCreated {
                id: Uuid::new_v4(),
                job_kind: "playwright_test".into(),
                status: "failed".into(),
                label: "x".into(),
                summary: None,
                artifacts: serde_json::Value::Null,
            })
            .unwrap();
        assert_eq!(hint.toast_severity.as_deref(), Some("error"));
        assert_eq!(hint.highlight_panel.as_deref(), Some("playwright"));
    }

    #[test]
    fn fast_query_emits_no_hint() {
        let c = Classifier::rules_only();
        let hint = c.classify(&ProjectEvent::DbQueryRun {
            query_id: None,
            duration_ms: 50,
            rows: 10,
            statement_kind: "select".into(),
        });
        assert!(hint.is_none());
    }

    #[test]
    fn agent_notification_passes_through() {
        let c = Classifier::rules_only();
        let hint = c
            .classify(&ProjectEvent::Notification {
                severity: "warning".into(),
                message: "attenzione".into(),
                panel: Some("playwright".into()),
                ttl_ms: None,
                run_id: None,
            })
            .unwrap();
        assert_eq!(hint.toast_msg.as_deref(), Some("attenzione"));
        assert_eq!(hint.highlight_panel.as_deref(), Some("playwright"));
    }
}
