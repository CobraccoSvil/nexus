"use client";

import type { DeepAnalysisInsights } from "../../../lib/api-client";
import { useThemeColors } from "../../../lib/theme";
import { cardStyle } from "./styles";

interface AnalysisInsightsCardProps {
  insights: DeepAnalysisInsights | null;
  insightsModel: string | null;
  insightsAt: string | null;
  analyzeBusy: boolean;
  deepAnalysisPhase: "idle" | "static" | "deep";
  onSendToChat?: (msg: string) => void;
  sentIssueIds: Set<number>;
  sentActionIds: Set<number>;
  setSentIssueIds: React.Dispatch<React.SetStateAction<Set<number>>>;
  setSentActionIds: React.Dispatch<React.SetStateAction<Set<number>>>;
}

export function AnalysisInsightsCard({
  insights,
  insightsModel,
  insightsAt,
  analyzeBusy,
  deepAnalysisPhase,
  onSendToChat,
  sentIssueIds,
  sentActionIds,
  setSentIssueIds,
  setSentActionIds,
}: AnalysisInsightsCardProps) {
  const tc = useThemeColors();

  return (
    <>
      {/* Placeholder mentre l'analisi profonda e' in corso: la vecchia card e'
          gia' stata svuotata da handleAnalyzeProject; mostriamo uno stato
          esplicito cosi' l'utente capisce che il sistema sta lavorando. */}
      {!insights && analyzeBusy && deepAnalysisPhase === "deep" && (
        <div style={{ ...cardStyle(tc), minWidth: 0, overflow: "hidden" }}>
          {/* keyframes inline: nessun foglio CSS globale modificato */}
          <style>{`@keyframes nx-pulse-dot {
            0%, 100% { opacity: 1; transform: scale(1); }
            50%      { opacity: 0.35; transform: scale(0.7); }
          }`}</style>
          <div style={{ display: "flex", alignItems: "center", gap: 8 }}>
            <span style={{
              width: 10, height: 10, borderRadius: "50%",
              background: "#60a5fa",
              animation: "nx-pulse-dot 1.4s ease-in-out infinite",
              flexShrink: 0,
            }} />
            <div style={{ color: tc.text, fontWeight: 600, fontSize: 12 }}>
              Analisi AI in corso...
            </div>
          </div>
          <div style={{ color: tc.textMuted, fontSize: 10, marginTop: 4, lineHeight: 1.4 }}>
            L&apos;agente sta leggendo i file di configurazione del progetto e
            valutando incoerenze, servizi rilevati e azioni consigliate.
            Tempo tipico: 30-60 secondi.
          </div>
        </div>
      )}

      {/* Card insights dell'agente agent.project.analyzer */}
      {insights && (
        <div style={{ ...cardStyle(tc), minWidth: 0, overflow: "hidden" }}>
          <div style={{ display: "flex", justifyContent: "space-between", alignItems: "center", gap: 6, marginBottom: 6, flexWrap: "wrap" }}>
            <div style={{ color: tc.text, fontWeight: 700 }}>Analisi AI del progetto</div>
            <div style={{ color: tc.textMuted, fontSize: 10, wordBreak: "break-all" }}>
              {insightsModel ? `${insightsModel}` : ""}{insightsAt ? ` · ${new Date(insightsAt).toLocaleString()}` : ""}
            </div>
          </div>
          {insights.project_summary && (
            <div style={{
              color: tc.textSecondary, fontSize: 11, marginBottom: 8, lineHeight: 1.5,
              wordBreak: "break-word", overflowWrap: "anywhere",
            }}>
              {insights.project_summary}
            </div>
          )}
          {insights.architecture && (
            <div style={{
              fontSize: 10, color: tc.textMuted, marginBottom: 8,
              wordBreak: "break-word", overflowWrap: "anywhere",
            }}>
              <span style={{ fontWeight: 600 }}>Architettura:</span> {insights.architecture.pattern}
              {insights.architecture.primary_languages && insights.architecture.primary_languages.length > 0 &&
                ` · ${insights.architecture.primary_languages.join(", ")}`}
            </div>
          )}

          {/* Servizi rilevati con modalita' di esecuzione consigliata */}
          {insights.services && insights.services.length > 0 && insights.services.some(s => s.recommended_run_mode) && (
            <div style={{ marginBottom: 8 }}>
              <div style={{ fontSize: 11, fontWeight: 600, color: tc.text, marginBottom: 4 }}>
                Servizi e modalita' di esecuzione
              </div>
              {insights.services.map((svc, idx) => {
                if (!svc.recommended_run_mode) return null;
                const modeColor = svc.recommended_run_mode === "native" ? "#22c55e"
                                : svc.recommended_run_mode === "docker" ? "#60a5fa"
                                : "#94a3b8";
                const modeLabel = svc.recommended_run_mode === "native" ? "nativo"
                                : svc.recommended_run_mode === "docker" ? "Docker"
                                : "scelta libera";
                return (
                  <div key={idx} style={{
                    border: `1px solid ${tc.border}`,
                    borderRadius: 3, padding: "5px 8px", marginBottom: 4, background: tc.bgCard,
                    minWidth: 0, maxWidth: "100%", overflow: "hidden",
                  }}>
                    <div style={{ display: "flex", alignItems: "center", gap: 6, flexWrap: "wrap" }}>
                      <span style={{
                        fontSize: 11, fontWeight: 600, color: tc.text,
                        wordBreak: "break-word", overflowWrap: "anywhere",
                      }}>{svc.name}</span>
                      {svc.port && (
                        <span style={{ fontSize: 9, color: tc.textMuted }}>:{svc.port}</span>
                      )}
                      <span style={{
                        fontSize: 9, color: modeColor,
                        background: `${modeColor}1c`,
                        border: `1px solid ${modeColor}55`,
                        borderRadius: 3, padding: "1px 5px",
                        fontFamily: 'var(--font-mono)',
                        whiteSpace: "nowrap",
                      }}>
                        consiglio: {modeLabel}
                      </span>
                    </div>
                    {svc.run_mode_rationale && (
                      <div style={{
                        fontSize: 10, color: tc.textSecondary, marginTop: 2, lineHeight: 1.4,
                        wordBreak: "break-word", overflowWrap: "anywhere",
                      }}>
                        {svc.run_mode_rationale}
                      </div>
                    )}
                    {svc.start_command && (
                      <code style={{
                        display: "block", marginTop: 3,
                        fontSize: 9, color: "#60a5fa", background: "rgba(96,165,250,0.08)",
                        padding: "2px 4px", borderRadius: 2, fontFamily: 'var(--font-mono)',
                        wordBreak: "break-all", overflowWrap: "anywhere",
                      }}>
                        {svc.start_command}
                      </code>
                    )}
                  </div>
                );
              })}
            </div>
          )}

          {/* Incoerenze di configurazione rilevate */}
          {insights.config_issues && insights.config_issues.length > 0 && (
            <div style={{ marginBottom: 8 }}>
              <div style={{ fontSize: 11, fontWeight: 600, color: tc.text, marginBottom: 4 }}>
                Incoerenze di configurazione ({insights.config_issues.length})
              </div>
              {insights.config_issues.map((iss, idx) => {
                const sevColor = iss.severity === "high" ? "#ef4444" : iss.severity === "medium" ? "#f59e0b" : "#94a3b8";
                const alreadySent = sentIssueIds.has(idx);
                const handleResolveWithNexus = () => {
                  if (!onSendToChat || alreadySent) return;
                  const filesList = (iss.files ?? []).map(f => `- \`${f}\``).join("\n");
                  // Niente istruzioni di autonomia (gia' nel dropdown chat),
                  // ma SI contesto di sistema: l'agente deve vedere TUTTE le
                  // incoerenze rilevate e i servizi consigliati, altrimenti
                  // risolve il punto in modo isolato e ignora la direzione
                  // generale del fix. Vedi caso reale: l'utente ha chiesto
                  // di eliminare Docker; senza contesto di sistema l'agente
                  // su una singola issue ricostruisce il setup Docker.
                  const otherIssues = insights.config_issues
                    .filter((_, i) => i !== idx)
                    .map(o => `  - [${o.severity.toUpperCase()}] ${o.title}${o.suggested_fix ? ` → ${o.suggested_fix}` : ""}`)
                    .join("\n");
                  const servicesContext = (insights.services ?? [])
                    .filter(s => s.recommended_run_mode)
                    .map(s => `  - ${s.name} (${s.type}${s.port ? `:${s.port}` : ""}) → modalita' consigliata: ${s.recommended_run_mode}${s.run_mode_rationale ? ` — ${s.run_mode_rationale}` : ""}`)
                    .join("\n");
                  const prompt = [
                    `Risolvi questo problema di configurazione del progetto rilevato dall'analisi AI.`,
                    ``,
                    `## Problema da risolvere`,
                    `**Severità**: ${iss.severity.toUpperCase()}`,
                    `**Titolo**: ${iss.title}`,
                    iss.description ? `**Descrizione**: ${iss.description}` : "",
                    filesList ? `**File coinvolti**:\n${filesList}` : "",
                    iss.suggested_fix ? `**Fix suggerito dall'analizzatore**: ${iss.suggested_fix}` : "",
                    ``,
                    otherIssues ? `## Contesto: altre incoerenze rilevate nello stesso report\n${otherIssues}\n\nRagiona in modo coerente con queste: applica un fix che vada nella stessa direzione del piano d'insieme, non un fix isolato che potrebbe contraddirle.` : "",
                    servicesContext ? `## Modalita' di esecuzione consigliate dall'analizzatore\n${servicesContext}\n\nSe il problema riguarda un servizio elencato sopra, rispetta la modalita' consigliata (native vs docker).` : "",
                    ``,
                    `Valida che il fix proposto sia corretto nel contesto complessivo e applicalo, segnalando alternative migliori se le rilevi.`,
                  ].filter(Boolean).join("\n");
                  onSendToChat(prompt);
                  // Memorizza l'invio per disabilitare il pulsante e mostrare
                  // visivamente che l'azione e' partita. Reset alla prossima analisi.
                  setSentIssueIds(prev => {
                    const next = new Set(prev);
                    next.add(idx);
                    return next;
                  });
                };
                // Stili condivisi per i blocchi testuali — gestiscono overflow e wrapping
                const wrapStyle: React.CSSProperties = {
                  wordBreak: "break-word",
                  overflowWrap: "anywhere",
                  whiteSpace: "pre-wrap",
                  minWidth: 0,
                };
                return (
                  <div key={idx} style={{
                    border: `1px solid ${tc.border}`, borderLeft: `3px solid ${sevColor}`,
                    borderRadius: 3, padding: "5px 8px", marginBottom: 4, background: tc.bgCard,
                    minWidth: 0, maxWidth: "100%", overflow: "hidden",
                  }}>
                    <div style={{ ...wrapStyle, fontSize: 11, fontWeight: 600, color: tc.text }}>
                      <span style={{ color: sevColor, fontSize: 9, marginRight: 4 }}>
                        [{iss.severity.toUpperCase()}]
                      </span>
                      {iss.title}
                    </div>
                    {iss.description && (
                      <div style={{ ...wrapStyle, fontSize: 10, color: tc.textSecondary, marginTop: 2, lineHeight: 1.4 }}>
                        {iss.description}
                      </div>
                    )}
                    {iss.suggested_fix && (
                      <div style={{
                        ...wrapStyle,
                        fontSize: 10, color: "#22c55e", marginTop: 3,
                        fontFamily: 'var(--font-mono)',
                      }}>
                        → {iss.suggested_fix}
                      </div>
                    )}
                    {onSendToChat && (
                      <div style={{ marginTop: 5, display: "flex", justifyContent: "flex-end" }}>
                        <button
                          onClick={handleResolveWithNexus}
                          disabled={alreadySent}
                          title={alreadySent
                            ? "Gia' inviato a Nexus — la chat sta processando o ha gia' completato. Rianalizza il progetto per ricaricare lo stato."
                            : "Invia il problema alla chat per farlo risolvere a Nexus"}
                          style={{
                            background: alreadySent ? "rgba(148,163,184,0.10)" : "rgba(96,165,250,0.12)",
                            border: alreadySent
                              ? "1px solid rgba(148,163,184,0.30)"
                              : "1px solid rgba(96,165,250,0.45)",
                            borderRadius: 3,
                            color: alreadySent ? tc.textMuted : "#60a5fa",
                            cursor: alreadySent ? "not-allowed" : "pointer",
                            padding: "2px 8px",
                            fontSize: 10,
                            fontWeight: 600,
                            whiteSpace: "nowrap",
                            opacity: alreadySent ? 0.7 : 1,
                          }}
                        >
                          {alreadySent ? "✓ inviato a Nexus" : "Risolvi con Nexus"}
                        </button>
                      </div>
                    )}
                  </div>
                );
              })}
            </div>
          )}

          {/* Azioni suggerite */}
          {insights.suggested_actions && insights.suggested_actions.length > 0 && (
            <div>
              <div style={{ fontSize: 11, fontWeight: 600, color: tc.text, marginBottom: 4 }}>
                Azioni suggerite
              </div>
              {insights.suggested_actions.slice(0, 5).map((act, idx) => {
                const alreadyRun = sentActionIds.has(idx);
                const handleRunWithNexus = () => {
                  if (!onSendToChat || alreadyRun) return;
                  // Stesso principio del pulsante "Risolvi con Nexus":
                  // passare contesto di sistema (altre azioni + servizi)
                  // affinche' l'agente non agisca in isolamento.
                  const otherActions = insights.suggested_actions
                    .filter((_, i) => i !== idx)
                    .slice(0, 4)
                    .map(a => `  ${a.priority}. ${a.title}${a.command ? ` (\`${a.command}\`)` : ""}`)
                    .join("\n");
                  const issuesContext = (insights.config_issues ?? [])
                    .map(o => `  - [${o.severity.toUpperCase()}] ${o.title}`)
                    .join("\n");
                  const servicesContext = (insights.services ?? [])
                    .filter(s => s.recommended_run_mode)
                    .map(s => `  - ${s.name} (${s.type}${s.port ? `:${s.port}` : ""}) → ${s.recommended_run_mode}`)
                    .join("\n");
                  const prompt = [
                    `Esegui questa azione suggerita dall'analisi AI del progetto.`,
                    ``,
                    `## Azione da eseguire`,
                    `**Titolo**: ${act.title}`,
                    act.command ? `**Comando proposto**: \`${act.command}\`` : "",
                    act.rationale ? `**Motivazione**: ${act.rationale}` : "",
                    ``,
                    issuesContext ? `## Contesto: incoerenze di config rilevate nel progetto\n${issuesContext}` : "",
                    otherActions ? `## Contesto: altre azioni nel piano d'insieme\n${otherActions}\n\nL'azione che esegui ora deve essere coerente con questo piano: non contraddire le altre azioni, non rifare lavoro inutile.` : "",
                    servicesContext ? `## Modalita' di esecuzione consigliate\n${servicesContext}` : "",
                    ``,
                    `Valida che il comando sia sicuro nel contesto del progetto attivo, eseguilo o adattalo se necessario, e riporta l'esito.`,
                  ].filter(Boolean).join("\n");
                  onSendToChat(prompt);
                  setSentActionIds(prev => {
                    const next = new Set(prev);
                    next.add(idx);
                    return next;
                  });
                };
                return (
                  <div key={idx} style={{
                    border: `1px solid ${tc.border}`,
                    borderRadius: 3, padding: "5px 8px", marginBottom: 4, background: tc.bgCard,
                    minWidth: 0, maxWidth: "100%", overflow: "hidden",
                  }}>
                    <div style={{ display: "flex", gap: 6, minWidth: 0 }}>
                      <span style={{ color: tc.textMuted, fontSize: 10, minWidth: 14, flexShrink: 0 }}>
                        {act.priority}.
                      </span>
                      <div style={{ flex: 1, minWidth: 0, overflow: "hidden" }}>
                        <div style={{
                          fontSize: 11, color: tc.text, fontWeight: 600,
                          wordBreak: "break-word", overflowWrap: "anywhere",
                        }}>{act.title}</div>
                        {act.command && (
                          <code style={{
                            display: "block",
                            fontSize: 9, color: "#60a5fa", background: "rgba(96,165,250,0.08)",
                            padding: "2px 4px", borderRadius: 2, fontFamily: 'var(--font-mono)',
                            wordBreak: "break-all", overflowWrap: "anywhere",
                            whiteSpace: "pre-wrap",
                            marginTop: 2,
                          }}>
                            {act.command}
                          </code>
                        )}
                        {act.rationale && (
                          <div style={{
                            fontSize: 9, color: tc.textMuted, marginTop: 2,
                            wordBreak: "break-word", overflowWrap: "anywhere",
                          }}>
                            {act.rationale}
                          </div>
                        )}
                      </div>
                    </div>
                    {onSendToChat && (
                      <div style={{ marginTop: 5, display: "flex", justifyContent: "flex-end" }}>
                        <button
                          onClick={handleRunWithNexus}
                          disabled={alreadyRun}
                          title={alreadyRun
                            ? "Gia' inviata a Nexus — la chat sta processando o ha gia' completato. Rianalizza il progetto per ricaricare lo stato."
                            : "Invia l'azione alla chat per farla eseguire da Nexus (con i tool del progetto)"}
                          style={{
                            background: alreadyRun ? "rgba(148,163,184,0.10)" : "rgba(34,197,94,0.12)",
                            border: alreadyRun
                              ? "1px solid rgba(148,163,184,0.30)"
                              : "1px solid rgba(34,197,94,0.45)",
                            borderRadius: 3,
                            color: alreadyRun ? tc.textMuted : "#22c55e",
                            cursor: alreadyRun ? "not-allowed" : "pointer",
                            padding: "2px 8px",
                            fontSize: 10,
                            fontWeight: 600,
                            whiteSpace: "nowrap",
                            opacity: alreadyRun ? 0.7 : 1,
                          }}
                        >
                          {alreadyRun ? "✓ inviata a Nexus" : "▶ Esegui con Nexus"}
                        </button>
                      </div>
                    )}
                  </div>
                );
              })}
            </div>
          )}
        </div>
      )}
    </>
  );
}
