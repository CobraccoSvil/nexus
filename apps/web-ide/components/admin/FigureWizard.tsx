"use client";

/**
 * FigureWizard — creazione guidata di una figura (lente) del consiglio.
 *
 * Una figura NON e' solo una riga di nexus_subagent_definitions: perche' il kind
 * sia davvero convocabile servono QUATTRO pezzi coerenti (definition, prompt
 * subagent.<kind>.base, purpose subagent_<kind>, appartenenza alla whitelist del
 * dispatcher). Il ramo "nuovo kind" dell'editor definitions ne creava uno solo,
 * lasciando il kind muto: qui si passa dall'endpoint transazionale
 * POST /orchestrator/figures che li crea tutti e quattro o nessuno.
 *
 * Il centro del form e' il prompt: la <lente> e' l'unica cosa che distingue una
 * figura dalle altre (tier, tool e limiti sono contorno). Per questo la textarea
 * nasce precompilata con lo scheletro XML dello schema standard (CLAUDE.md sez. D,
 * come i prompt seedati in mig 0546/0554) e il wizard non lascia creare una figura
 * col segnaposto della lente ancora intatto.
 *
 * Selezione modello TIER-ONLY: qui non si sceglie mai un nome di modello. Il
 * purpose nasce con provider/model_id vuoti e il modello concreto lo risolve
 * best_model_for_tier dal catalog a ogni convocazione (regola G).
 *
 * Riusabile: montato sia nell'editor definitions (/admin/orchestrator/subagents)
 * sia nella pagina del consiglio (/admin/council).
 */

import { useMemo, useState, type CSSProperties, type ReactNode } from "react";

import { ApiError } from "../../lib/api/_shared";
import { createFigure, type CreateFigureResult } from "../../lib/api/agent";
import { useThemeColors } from "../../lib/theme";
import { AdminModal } from "./AdminModal";
// Vocabolario tier: punto unico TS gia' esistente (regola L). L'opzione
// value="" ("Statico (modello fisso)") viene esclusa per costruzione: una figura
// e' tier-only, un modello fisso qui sarebbe una configurazione non ammessa.
import { PURPOSE_TIER_OPTIONS } from "../settings/routing-config/shared";
import { useI18n } from "../../lib/i18n";

type ThemeColors = ReturnType<typeof useThemeColors>;

// ── Vocabolari e preset ──────────────────────────────────────────────────────

const TIER_OPTIONS = PURPOSE_TIER_OPTIONS.filter((o) => o.value !== "");

/**
 * Preset read-only del consiglio: l'array delle figure advisory (mig 0546) piu'
 * advisory_verdict, il tool con cui la figura chiude il proprio giudizio in forma
 * strutturata (mig 0548/0554). Senza advisory_verdict in whitelist la figura non
 * potrebbe emettere il verdetto e il consiglio la conterebbe come voce morta.
 */
const ADVISORY_TOOLS: readonly string[] = [
  "read_file",
  "search_in_files",
  "list_files",
  "search_codebase_semantic",
  "recall_context",
  "nexus_search_semantic",
  "knowledge_search",
  "advisory_verdict",
];

/** Preset esecutivo: default storico del ramo "nuovo kind" dell'editor definitions. */
const EXECUTIVE_TOOLS: readonly string[] = ["list_files", "read_file", "search_in_files"];

/** Segnaposto della lente: se resta nel prompt, la figura non ha una prospettiva propria. */
const LENTE_PLACEHOLDER =
  "QUI DESCRIVI LA PROSPETTIVA UNICA DI QUESTA FIGURA: cosa vede lei che le altre non vedono.";

const KIND_RE = /^[a-z][a-z0-9_]{1,63}$/;

// ── Validazione pura (testabile, indipendente da React) ──────────────────────

export type KindCheck = { ok: true } | { ok: false; reason: string };

/**
 * Valida il kind contro ^[a-z][a-z0-9_]{1,63}$ spiegando QUALE vincolo e' violato:
 * un messaggio "kind non valido" costringerebbe l'utente a indovinare la regola.
 * Il backend resta autoritativo (stessa regex server-side): questa e' guida, non
 * il punto di enforcement.
 */
export function validateKind(kind: string): KindCheck {
  if (kind.length === 0) return { ok: false, reason: "Inserisci un identificatore." };
  if (KIND_RE.test(kind)) return { ok: true };
  if (!/^[a-z]/.test(kind)) {
    return { ok: false, reason: "Deve iniziare con una lettera minuscola (a-z)." };
  }
  if (kind.length < 2) return { ok: false, reason: "Deve essere lungo almeno 2 caratteri." };
  if (kind.length > 64) {
    return { ok: false, reason: `Massimo 64 caratteri (attuale: ${kind.length}).` };
  }
  const bad = Array.from(new Set(kind.split("").filter((c) => !/[a-z0-9_]/.test(c))));
  if (bad.length > 0) {
    return {
      ok: false,
      reason: `Caratteri non ammessi: ${bad.map((c) => (c === " " ? "spazio" : c)).join(" ")}. Sono ammesse solo lettere minuscole, cifre e underscore.`,
    };
  }
  return { ok: false, reason: "Formato non valido." };
}

/**
 * Scheletro XML dello schema standard (CLAUDE.md sez. D), modellato sui prompt
 * seedati delle figure del consiglio. Il canale e' FUORI CHAT (il sub-run non
 * eredita alcuna modalita' UI): autonomia, anti-loop e output format devono
 * essere espliciti nel prompt.
 */
export function buildPromptSkeleton(kind: string, description: string, advisory: boolean): string {
  const name = kind.trim() || "<kind>";
  const desc = description.trim();
  const role = advisory
    ? `Sei ${name} nel consiglio di analisi Nexus.${desc ? ` ${desc}` : ""} Analizzi una richiesta di sviluppo dalla tua prospettiva. NON scrivi ne' esegui codice: osservi e consigli.`
    : `Sei ${name}, sub-agente esecutivo di Nexus.${desc ? ` ${desc}` : ""} Ricevi un task delimitato e lo porti a termine.`;

  const autonomia = advisory
    ? `- Tool read-only (${ADVISORY_TOOLS.filter((t) => t !== "advisory_verdict").join(", ")}).
- Cerca i punti unici gia' esistenti nel codebase prima di dichiarare un problema: un difetto gia' presidiato non e' un rischio.`
    : `- Tool assegnati dalla whitelist della definition. Non chiedere conferma: il task e' gia' delimitato.
- Cerca il punto unico esistente prima di scrivere logica nuova (regola L).`;

  const outputFormat = advisory
    ? `Chiudi SEMPRE chiamando advisory_verdict come ultimissima azione: verdict = proceed | proceed_with_changes | block; requirements = vincoli azionabili per chi esegue; risks = [{severity: alta|media|bassa, description con evidenza}]; recommendations = suggerimenti non vincolanti. Niente dump di file.`
    : `final_answer strutturato: cosa hai fatto, i file toccati, l'esito della verifica. Niente dump di file.`;

  const principi = advisory
    ? `- VETA (verdict=block) SOLO con evidenza concreta e verificabile, mai su un sospetto.
- Regola M: valuta gli esiti da segnali strutturati (exit code, campi enum), mai dal testo in prosa.`
    : `- Regola H: vai alla causa radice, mai una toppa che maschera il sintomo.
- Regola M: valuta gli esiti da segnali strutturati (exit code, campi enum), mai dal testo in prosa.`;

  return `<role>${role}</role>

<contesto>
Richiesta isolata + contesto (memoria_progetto, rationale_parent).${advisory ? " Altre figure analizzano la stessa richiesta in parallelo; il coordinatore sintetizza i verdetti." : ""}
</contesto>

<lente>
${LENTE_PLACEHOLDER}
-
-
-
</lente>

<autonomia>
${autonomia}
</autonomia>

<principi_nexus>
${principi}
</principi_nexus>

<anti_loop>
Un solo giro di analisi mirata. Concludi appena hai l'evidenza: niente letture esplorative ripetute sugli stessi file.
</anti_loop>

<output_format>
${outputFormat}
</output_format>`;
}

/** Sezioni XML che il backend pretende nel prompt. La <lente> e' quella che da' senso alla figura. */
const REQUIRED_SECTIONS = [
  "role",
  "contesto",
  "lente",
  "autonomia",
  "principi_nexus",
  "anti_loop",
  "output_format",
] as const;

export function missingSections(prompt: string): string[] {
  return REQUIRED_SECTIONS.filter((s) => !(prompt.includes(`<${s}>`) && prompt.includes(`</${s}>`)));
}

// ── Componente ───────────────────────────────────────────────────────────────

export interface FigureWizardProps {
  open: boolean;
  onClose: () => void;
  /** Invocato dopo una creazione riuscita: il chiamante ricarica le proprie liste. */
  onCreated?: (result: CreateFigureResult) => void;
}

type Step = 1 | 2 | 3;

export function FigureWizard({ open, onClose, onCreated }: FigureWizardProps) {
  const { t } = useI18n();
  const tc = useThemeColors();

  const [step, setStep] = useState<Step>(1);
  const [kind, setKind] = useState("");
  const [description, setDescription] = useState("");
  const [advisory, setAdvisory] = useState(true);
  const [tier, setTier] = useState("medium");
  // Limiti opzionali: vuoto = non spedito, il default resta UNO solo (il backend).
  // Ricopiarne il valore qui creerebbe una seconda fonte di verita' (regola G).
  const [maxIterations, setMaxIterations] = useState("");
  const [timeoutS, setTimeoutS] = useState("");
  // Il prompt segue kind/description/advisory finche' l'utente non lo tocca:
  // dopo la prima battitura e' suo e non viene piu' rigenerato sotto le dita.
  const [promptDraft, setPromptDraft] = useState("");
  const [promptTouched, setPromptTouched] = useState(false);
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);
  /** Status HTTP dell'ultimo errore: segnale strutturato per il suggerimento (regola M). */
  const [errorStatus, setErrorStatus] = useState<number | null>(null);
  const [created, setCreated] = useState<CreateFigureResult | null>(null);

  const skeleton = useMemo(
    () => buildPromptSkeleton(kind, description, advisory),
    [kind, description, advisory],
  );
  const prompt = promptTouched ? promptDraft : skeleton;

  const kindCheck = validateKind(kind);
  const tools = advisory ? ADVISORY_TOOLS : EXECUTIVE_TOOLS;
  const lensPending = prompt.includes(LENTE_PLACEHOLDER);
  const missing = missingSections(prompt);
  const step1Ok = kindCheck.ok && description.trim().length > 0;
  const step2Ok = !lensPending && missing.length === 0 && prompt.trim().length > 0;

  // Anteprima delle derivazioni canoniche: le chiavi vere le calcola il backend
  // (punto unico) e le ritorna nella risposta, che e' quella che mostriamo dopo.
  const promptKeyPreview = `subagent.${kind || "<kind>"}.base`;
  const purposePreview = `subagent_${kind || "<kind>"}`;

  const reset = () => {
    setStep(1);
    setKind("");
    setDescription("");
    setAdvisory(true);
    setTier("medium");
    setMaxIterations("");
    setTimeoutS("");
    setPromptDraft("");
    setPromptTouched(false);
    setBusy(false);
    setError(null);
    setErrorStatus(null);
    setCreated(null);
  };

  const handleClose = () => {
    if (busy) return; // creazione in volo: non si chiude a meta' transazione
    reset();
    onClose();
  };

  const submit = async () => {
    setBusy(true);
    setError(null);
    setErrorStatus(null);
    try {
      const result = await createFigure({
        kind,
        description: description.trim(),
        advisory,
        tier,
        prompt_content: prompt,
        prompt_title: `${advisory ? "Consiglio" : "Sub-agent"}: ${kind}`,
        tool_whitelist: [...tools],
        ...(maxIterations.trim() ? { max_iterations: Number(maxIterations) } : {}),
        ...(timeoutS.trim() ? { timeout_s: Number(timeoutS) } : {}),
      });
      setCreated(result);
      onCreated?.(result);
    } catch (e) {
      if (e instanceof ApiError) {
        setErrorStatus(e.status);
        setError(e.message);
      } else {
        setError(e instanceof Error ? e.message : "Errore creazione figura");
      }
    } finally {
      setBusy(false);
    }
  };

  const title = created
    ? `Figura '${created.kind}' creata`
    : `Nuova figura — ${step}/3 ${step === 1 ? "Identita'" : step === 2 ? "Lente" : "Riepilogo"}`;

  return (
    /* maxWidth ampio: la textarea del prompt e' il centro del form, non un dettaglio. */
    <AdminModal open={open} onClose={handleClose} title={title} maxWidth={880}>
      {created ? (
        <SuccessPanel tc={tc} result={created} advisory={advisory} onClose={handleClose} />
      ) : (
        <div style={{ display: "flex", flexDirection: "column", gap: 16 }}>
          <StepBar tc={tc} step={step} />

          {step === 1 ? (
            <div style={{ display: "flex", flexDirection: "column", gap: 14 }}>
              <Field
                tc={tc}
                label="Identificatore (kind)"
                hint="Nome canonico usato dal dispatcher e derivato nelle chiavi di prompt e purpose. Non e' modificabile dopo la creazione."
              >
                <input
                  value={kind}
                  onChange={(e) => setKind(e.target.value)}
                  placeholder={t("admin.esDataArchitect")}
                  autoFocus
                  spellCheck={false}
                  style={{ ...fieldStyle(tc), fontFamily: "var(--font-mono)" }}
                />
                {kind.length > 0 && !kindCheck.ok ? (
                  <span style={{ fontSize: 11, color: tc.error }}>{kindCheck.reason}</span>
                ) : kindCheck.ok ? (
                  <span style={{ fontSize: 11, color: tc.textMuted }}>
                    {t("admin.prompt")} <code style={{ fontFamily: "var(--font-mono)" }}>{promptKeyPreview}</code> · purpose{" "}
                    <code style={{ fontFamily: "var(--font-mono)" }}>{purposePreview}</code>
                  </span>
                ) : (
                  <span style={{ fontSize: 11, color: tc.textMuted }}>
                    Minuscole, cifre e underscore; da 2 a 64 caratteri; iniziale a-z.
                  </span>
                )}
              </Field>

              <Field
                tc={tc}
                label="Descrizione"
                hint="Usata dal dispatcher per la delega per descrizione: dice in una riga quando questa figura serve."
              >
                <textarea
                  value={description}
                  onChange={(e) => setDescription(e.target.value)}
                  rows={2}
                  placeholder={t("admin.esValutaIlModello")}
                  style={{ ...fieldStyle(tc), fontFamily: "inherit", resize: "vertical" }}
                />
              </Field>

              <Field tc={tc} label="Natura della figura">
                <div style={{ display: "flex", flexDirection: "column", gap: 6 }}>
                  <label style={radioRowStyle}>
                    <input
                      type="radio"
                      name="figure-advisory"
                      checked={advisory}
                      onChange={() => setAdvisory(true)}
                    />
                    <span>
                      <strong>{t("admin.advisory")}</strong> — read-only: osserva e consiglia, chiude con{" "}
                      <code style={{ fontFamily: "var(--font-mono)" }}>advisory_verdict</code>.
                    </span>
                  </label>
                  <label style={radioRowStyle}>
                    <input
                      type="radio"
                      name="figure-advisory"
                      checked={!advisory}
                      onChange={() => setAdvisory(false)}
                    />
                    <span>
                      <strong>{t("admin.esecutivo")}</strong> — sub-agente che porta a termine un task delimitato.
                    </span>
                  </label>
                </div>
              </Field>

              <Field
                tc={tc}
                label="Tool whitelist"
                hint={
                  advisory
                    ? "Preset advisory read-only: una figura advisory e' read-only per definizione, i tool di scrittura sono esclusi."
                    : "Preset esecutivo. La whitelist si affina dall'editor definitions dopo la creazione."
                }
              >
                <div style={{ display: "flex", gap: 4, flexWrap: "wrap" }}>
                  {tools.map((t) => (
                    <span
                      key={t}
                      style={{
                        fontFamily: "var(--font-mono)",
                        fontSize: 10,
                        padding: "2px 6px",
                        borderRadius: 4,
                        background: tc.bgCard,
                        border: `1px solid ${tc.border}`,
                        color: t === "advisory_verdict" ? tc.accent : tc.textMuted,
                      }}
                    >
                      {t}
                    </span>
                  ))}
                </div>
              </Field>

              <div style={{ display: "grid", gridTemplateColumns: "1fr 1fr 1fr", gap: 10 }}>
                <Field tc={tc} label="Tier">
                  <select value={tier} onChange={(e) => setTier(e.target.value)} style={fieldStyle(tc)}>
                    {TIER_OPTIONS.map((o) => (
                      <option key={o.value} value={o.value}>
                        {o.label}
                      </option>
                    ))}
                  </select>
                </Field>
                <Field tc={tc} label="Max iterazioni">
                  <input
                    value={maxIterations}
                    onChange={(e) => setMaxIterations(e.target.value)}
                    inputMode="numeric"
                    placeholder={t("admin.predefinito")}
                    style={fieldStyle(tc)}
                  />
                </Field>
                <Field tc={tc} label="Timeout (s)">
                  <input
                    value={timeoutS}
                    onChange={(e) => setTimeoutS(e.target.value)}
                    inputMode="numeric"
                    placeholder={t("admin.predefinito")}
                    style={fieldStyle(tc)}
                  />
                </Field>
              </div>
              <div style={{ fontSize: 11, color: tc.textMuted, marginTop: -6 }}>
                {t("admin.ilTierSceglieLa")} <em>fascia di capacita'</em>, mai un modello: il modello concreto lo
                risolve best_model_for_tier dal catalog a ogni convocazione, tenendo conto di capability
                e cooldown. Iterazioni e timeout vuoti usano i valori predefiniti del backend.
              </div>
            </div>
          ) : null}

          {step === 2 ? (
            <div style={{ display: "flex", flexDirection: "column", gap: 10 }}>
              <div style={{ fontSize: 12, color: tc.textMuted }}>
                {t("admin.questoPrompt")} <strong>e'</strong> la figura. Il blocco <code style={{ fontFamily: "var(--font-mono)" }}>&lt;lente&gt;</code> e'
                l'unica cosa che la distingue dalle altre: se descrive una prospettiva che un'altra figura
                ha gia', il consiglio otterra' due voci che dicono la stessa cosa.
              </div>
              <textarea
                value={prompt}
                onChange={(e) => {
                  setPromptTouched(true);
                  setPromptDraft(e.target.value);
                }}
                rows={22}
                spellCheck={false}
                style={{
                  width: "100%",
                  fontFamily: "var(--font-mono)",
                  fontSize: 12,
                  lineHeight: 1.5,
                  padding: 12,
                  background: tc.bgCard,
                  color: tc.text,
                  border: `1px solid ${lensPending ? tc.warning : tc.border}`,
                  borderRadius: 8,
                  resize: "vertical",
                  boxSizing: "border-box",
                }}
              />
              <div style={{ display: "flex", alignItems: "center", gap: 10, flexWrap: "wrap" }}>
                {promptTouched ? (
                  <button
                    type="button"
                    onClick={() => {
                      setPromptTouched(false);
                      setPromptDraft("");
                    }}
                    style={btnStyle(tc, "ghost")}
                    title={t("admin.rigeneraLoScheletroDallo")}
                  >
                    {t("admin.ripristinaScheletro")}
                  </button>
                ) : (
                  <span style={{ fontSize: 11, color: tc.textMuted }}>
                    Lo scheletro si aggiorna con kind e natura della figura finche' non lo modifichi.
                  </span>
                )}
                {lensPending ? (
                  <span style={{ fontSize: 11, color: tc.warning }}>
                    La lente contiene ancora il segnaposto: sostituiscilo con la prospettiva della figura.
                  </span>
                ) : null}
                {missing.length > 0 ? (
                  <span style={{ fontSize: 11, color: tc.error }}>
                    Sezioni mancanti o non chiuse: {missing.map((s) => `<${s}>`).join(" ")}
                  </span>
                ) : null}
              </div>
            </div>
          ) : null}

          {step === 3 ? (
            <div style={{ display: "flex", flexDirection: "column", gap: 12 }}>
              <div style={{ fontSize: 12, color: tc.textMuted }}>
                La creazione e' una sola transazione: o nascono tutti e quattro i pezzi, o nessuno.
              </div>
              <SummaryItem
                tc={tc}
                n={1}
                title={t("admin.definition")}
                target="nexus_subagent_definitions"
                lines={[
                  `kind: ${kind}`,
                  `tool whitelist: ${tools.join(", ")}`,
                  `limiti: ${maxIterations.trim() || "predefinito"} iterazioni · ${timeoutS.trim() || "predefinito"} s`,
                ]}
              />
              <SummaryItem
                tc={tc}
                n={2}
                title={t("admin.prompt")}
                target="nexus_prompt_templates"
                lines={[`chiave: ${promptKeyPreview}`, `versione iniziale: v1`]}
              />
              <SummaryItem
                tc={tc}
                n={3}
                title={t("admin.purpose")}
                target="nexus_purpose_model"
                lines={[
                  `purpose: ${purposePreview}`,
                  `tier: ${tier} (provider e modello vuoti: li sceglie best_model_for_tier)`,
                ]}
              />
              <SummaryItem
                tc={tc}
                n={4}
                title={t("admin.whitelistDispatcher")}
                target="orchestrator.subagent_kinds_whitelist"
                lines={[`append di '${kind}' — senza questo il kind esisterebbe ma non sarebbe convocabile`]}
              />
              {advisory ? (
                <div style={{ fontSize: 11, color: tc.textMuted, borderTop: `1px solid ${tc.border}`, paddingTop: 10 }}>
                  La figura non entra da sola nel roster: per farla convocare dal consiglio aggiungila al
                  gruppo Base o Infrastruttura dalla sezione Composizione della pagina Consiglio.
                </div>
              ) : null}
              {error ? (
                <div
                  style={{
                    fontSize: 12,
                    color: tc.error,
                    padding: "8px 10px",
                    border: `1px solid ${tc.error}`,
                    borderRadius: 6,
                  }}
                >
                  {error}
                  {errorStatus === 409 ? (
                    <div style={{ color: tc.textMuted, marginTop: 4 }}>
                      Esiste gia' un elemento con questo identificatore. Torna al passo 1 e scegline un altro.
                    </div>
                  ) : null}
                  {errorStatus === 400 ? (
                    <div style={{ color: tc.textMuted, marginTop: 4 }}>
                      Il backend ha rifiutato i dati: correggi il passo indicato. Nessuna riga e' stata creata.
                    </div>
                  ) : null}
                </div>
              ) : null}
            </div>
          ) : null}

          {/* Navigazione */}
          <div
            style={{
              display: "flex",
              justifyContent: "space-between",
              gap: 8,
              borderTop: `1px solid ${tc.border}`,
              paddingTop: 12,
            }}
          >
            <button type="button" onClick={handleClose} disabled={busy} style={btnStyle(tc, "ghost", busy)}>
              {t("admin.annulla")}
            </button>
            <div style={{ display: "flex", gap: 8 }}>
              {step > 1 ? (
                <button
                  type="button"
                  onClick={() => setStep((s) => (s === 3 ? 2 : 1))}
                  disabled={busy}
                  style={btnStyle(tc, "ghost", busy)}
                >
                  {t("admin.indietro")}
                </button>
              ) : null}
              {step < 3 ? (
                <button
                  type="button"
                  onClick={() => setStep((s) => (s === 1 ? 2 : 3))}
                  disabled={step === 1 ? !step1Ok : !step2Ok}
                  style={btnStyle(tc, "primary", step === 1 ? !step1Ok : !step2Ok)}
                  title={
                    step === 1 && !step1Ok
                      ? "Servono un identificatore valido e una descrizione"
                      : step === 2 && !step2Ok
                        ? "Completa la lente e le sezioni obbligatorie"
                        : undefined
                  }
                >
                  {t("admin.avanti")}
                </button>
              ) : (
                <button
                  type="button"
                  onClick={() => void submit()}
                  disabled={busy || !step1Ok || !step2Ok}
                  style={btnStyle(tc, "primary", busy || !step1Ok || !step2Ok)}
                >
                  {busy ? "Creazione…" : "Crea figura"}
                </button>
              )}
            </div>
          </div>
        </div>
      )}
    </AdminModal>
  );
}

// ── Sotto-componenti di presentazione ────────────────────────────────────────

function StepBar({ tc, step }: { tc: ThemeColors; step: Step }) {
  const labels: Array<[Step, string]> = [
    [1, "Identita'"],
    [2, "Lente"],
    [3, "Riepilogo"],
  ];
  return (
    <div style={{ display: "flex", gap: 6 }}>
      {labels.map(([n, label]) => (
        <div
          key={n}
          style={{
            flex: 1,
            padding: "4px 8px",
            borderRadius: 6,
            fontSize: 11,
            fontWeight: 600,
            textAlign: "center",
            background: n === step ? tc.accentBg : tc.bgCard,
            color: n === step ? tc.accent : tc.textMuted,
            border: `1px solid ${n === step ? tc.accent : tc.border}`,
          }}
        >
          {n}. {label}
        </div>
      ))}
    </div>
  );
}

function Field({
  tc,
  label,
  hint,
  children,
}: {
  tc: ThemeColors;
  label: string;
  hint?: string;
  children: ReactNode;
}) {
  return (
    <label style={{ display: "flex", flexDirection: "column", gap: 4 }}>
      <span style={{ fontSize: 12, fontWeight: 600, color: tc.text }}>{label}</span>
      {hint ? <span style={{ fontSize: 11, color: tc.textMuted }}>{hint}</span> : null}
      {children}
    </label>
  );
}

function SummaryItem({
  tc,
  n,
  title,
  target,
  lines,
}: {
  tc: ThemeColors;
  n: number;
  title: string;
  target: string;
  lines: string[];
}) {
  return (
    <div style={{ display: "flex", gap: 10, alignItems: "flex-start" }}>
      <span
        style={{
          flexShrink: 0,
          width: 20,
          height: 20,
          borderRadius: 10,
          background: tc.accentBg,
          color: tc.accent,
          fontSize: 11,
          fontWeight: 700,
          display: "flex",
          alignItems: "center",
          justifyContent: "center",
        }}
      >
        {n}
      </span>
      <div style={{ minWidth: 0 }}>
        <div style={{ fontSize: 12, fontWeight: 600, color: tc.text }}>
          {title}{" "}
          <code style={{ fontFamily: "var(--font-mono)", fontSize: 10, color: tc.textMuted, fontWeight: 400 }}>
            {target}
          </code>
        </div>
        {lines.map((l) => (
          <div key={l} style={{ fontSize: 11, color: tc.textMuted, wordBreak: "break-word" }}>
            {l}
          </div>
        ))}
      </div>
    </div>
  );
}

function SuccessPanel({
  tc,
  result,
  advisory,
  onClose,
}: {
  tc: ThemeColors;
  result: CreateFigureResult;
  advisory: boolean;
  onClose: () => void;
}) {
  const { t } = useI18n();
  return (
    <div style={{ display: "flex", flexDirection: "column", gap: 12 }}>
      <div style={{ fontSize: 12, color: tc.textMuted }}>
        {t("admin.creatiIQuattroPezzi")}
      </div>
      <ul style={{ margin: 0, paddingLeft: 18, fontSize: 12, display: "flex", flexDirection: "column", gap: 4 }}>
        <li>
          {t("admin.definition")} <code style={{ fontFamily: "var(--font-mono)" }}>{result.kind}</code>
        </li>
        <li>
          {t("admin.prompt")} <code style={{ fontFamily: "var(--font-mono)" }}>{result.prompt_key}</code>
        </li>
        <li>
          {t("admin.purpose")} <code style={{ fontFamily: "var(--font-mono)" }}>{result.purpose}</code>
        </li>
        <li>
          Whitelist dispatcher:{" "}
          <span style={{ color: result.whitelisted ? tc.success : tc.warning }}>
            {result.whitelisted ? "inserito" : "NON inserito — il kind non e' convocabile"}
          </span>
        </li>
      </ul>
      {advisory ? (
        <div style={{ fontSize: 11, color: tc.textMuted }}>
          Per farla convocare dal consiglio, aggiungila a un gruppo dalla sezione Composizione.
        </div>
      ) : null}
      <div style={{ display: "flex", justifyContent: "flex-end" }}>
        <button type="button" onClick={onClose} style={btnStyle(tc, "primary")}>
          {t("admin.chiudi")}
        </button>
      </div>
    </div>
  );
}

// ── Stili locali (stessi helper delle pagine admin) ──────────────────────────

const radioRowStyle: CSSProperties = {
  display: "flex",
  gap: 6,
  alignItems: "flex-start",
  fontSize: 12,
  cursor: "pointer",
};

function fieldStyle(tc: ThemeColors): CSSProperties {
  return {
    padding: "6px 10px",
    background: tc.bgCard,
    color: tc.text,
    border: `1px solid ${tc.border}`,
    borderRadius: 6,
    fontSize: 12,
    width: "100%",
    boxSizing: "border-box",
  };
}

function btnStyle(tc: ThemeColors, variant: "primary" | "ghost", disabled = false): CSSProperties {
  const base: CSSProperties = {
    padding: "5px 12px",
    borderRadius: 6,
    fontSize: 12,
    fontWeight: 600,
    cursor: disabled ? "default" : "pointer",
    opacity: disabled ? 0.5 : 1,
    border: `1px solid ${tc.border}`,
    background: tc.bgCard,
    color: tc.text,
  };
  if (variant === "primary") {
    return { ...base, background: tc.accent, borderColor: tc.accent, color: "#fff" };
  }
  return base;
}
