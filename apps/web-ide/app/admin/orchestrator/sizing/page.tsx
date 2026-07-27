"use client";

/**
 * Pagina admin: Dimensionamento dell'orchestrazione (fase 8 del paradigma
 * "orchestrazione dimensionata dal problema", mig 0602/0604/0605).
 *
 * A cosa serve: rispondere alla domanda "perche' questo run ha convocato 3
 * figure invece di 5?" e poter cambiare la risposta senza toccare il DB a mano.
 * Le stesse chiavi sono editabili da /admin/settings/orchestrator, ma li' sono
 * una lista piatta di stringhe: una MATRICE classe x panel dentro un JSON e un
 * ordine di degrado dentro un CSV sono illeggibili in quella forma.
 *
 * Nessun endpoint nuovo (regola L): legge le categorie 'orchestrator' e 'agent'
 * con listAdminSettingsByCategory e scrive con updateAdminSetting — gli stessi
 * punti unici usati dalle altre pagine admin. Il JSON dei profili e il CSV
 * dell'ordine di degrado sono dettagli di SERIALIZZAZIONE: qui si vedono una
 * griglia di numeri e una lista ordinata, ma il valore salvato resta la forma
 * canonica che il resolver legge (regola N).
 *
 * Onesta' della pagina: un numero che oggi non ha effetto viene spiegato, non
 * nascosto. I profili 'low' non convocano panel (il gate deliberate a monte li
 * esclude), gli avvocati entrano solo se il consiglio dichiara una decisione
 * contesa, e ogni colonna il cui panel e' spento lo dichiara.
 */

import { useCallback, useMemo, useState, type CSSProperties, type ReactNode } from "react";

import { AdminPageHeader } from "../../../../components/admin/AdminPageHeader";
import {
  listAdminSettingsByCategory,
  updateAdminSetting,
  type AdminSettingEntry,
} from "../../../../lib/api/admin-settings";
import { useThemeColors } from "../../../../lib/theme";
import { useListData } from "../../../../lib/use-list-data";

type ThemeColors = ReturnType<typeof useThemeColors>;

// ── Chiavi (mig 0602/0604/0605) ──────────────────────────────────────────────

const SIZING_ENABLED_KEY = "orchestrator.sizing_enabled";
const PRIORITY_KEY = "orchestrator.sizing.panel_priority";
const SETTINGS_ORCHESTRATOR_HREF = "/admin/settings/orchestrator";

/** Classi di complessita' dichiarate dal classificatore (regola N: identificatori canonici). */
type ComplexityClass = "low" | "medium" | "high";
const COMPLEXITY_CLASSES: readonly ComplexityClass[] = ["low", "medium", "high"] as const;

/** Campi del JSON di orchestrator.sizing_profile_<classe> (mig 0602). */
type ProfileField = "council_figures" | "reviewers" | "providers" | "advocates";

/** Panel canonici: stessi identificatori del CSV panel_priority e del resolver. */
type PanelId = "council" | "multi_provider" | "debate" | "review";
const PANEL_ORDER_DEFAULT: readonly PanelId[] = [
  "council",
  "multi_provider",
  "debate",
  "review",
] as const;

const PANEL_LABEL: Record<PanelId, string> = {
  council: "Consiglio",
  multi_provider: "Multi-provider",
  debate: "Dibattito",
  review: "Review adversariale",
};

interface ColumnSpec {
  field: ProfileField;
  panel: PanelId;
  label: string;
  /** Una riga: cosa decide questo numero. */
  help: string;
  /** Tetto di sicurezza che lo clampa (sezione Backstop). */
  backstopKey: string;
  /** Setting che accende il panel: a false la colonna non ha effetto. */
  enabledKey: string;
}

const COLUMNS: readonly ColumnSpec[] = [
  {
    field: "council_figures",
    panel: "council",
    label: "Consiglieri",
    help: "Figure advisory convocate PRIMA del run: studiano il task in parallelo e votano un verdetto.",
    backstopKey: "orchestrator.council_max_figures",
    enabledKey: "orchestrator.council_enabled",
  },
  {
    field: "reviewers",
    panel: "review",
    label: "Revisori",
    help: "Revisori adversariali convocati DOPO il run sul diff prodotto.",
    backstopKey: "orchestrator.review_panel_size",
    enabledKey: "orchestrator.review_panel_autoconvene_enabled",
  },
  {
    field: "providers",
    panel: "multi_provider",
    label: "Provider",
    help: "Provider di case diverse interrogati in parallelo sullo stesso task, per confrontarne le risposte.",
    backstopKey: "orchestrator.multi_provider_max_providers",
    enabledKey: "orchestrator.multi_provider_enabled",
  },
  {
    field: "advocates",
    panel: "debate",
    label: "Avvocati",
    help: "Avvocati di tesi contrapposte: uno per opzione, difendono la posizione assegnata con prove dal codice.",
    backstopKey: "orchestrator.debate_max_advocates",
    enabledKey: "orchestrator.debate_enabled",
  },
];

const CLASS_LABEL: Record<ComplexityClass, string> = {
  low: "Bassa",
  medium: "Media",
  high: "Alta",
};

const CLASS_HELP: Record<ComplexityClass, string> = {
  low: "Task semplici. Oggi il gate a monte non convoca panel sui task low: questi numeri restano senza effetto finche' quel gate non cambia.",
  medium: "Task ordinari con impatto reale sul progetto.",
  high: "Task complessi o a impatto architetturale: e' qui che il dibattito ha senso.",
};

interface BackstopSpec {
  key: string;
  label: string;
  help: string;
}

const BACKSTOPS: readonly BackstopSpec[] = [
  {
    key: "orchestrator.council_max_figures",
    label: "Consiglieri",
    help: "Tetto assoluto sulle figure del consiglio: il piano non puo' superarlo, qualunque numero dica il profilo.",
  },
  {
    key: "orchestrator.review_panel_size",
    label: "Revisori",
    help: "Tetto sui revisori adversariali convocati a fine run.",
  },
  {
    key: "orchestrator.multi_provider_max_providers",
    label: "Provider",
    help: "Tetto sui provider interrogati in parallelo nel panel multi-provider.",
  },
  {
    key: "orchestrator.debate_max_advocates",
    label: "Avvocati",
    help: "Tetto sugli avvocati di un dibattito. Sotto 2 il dibattito non si tiene: senza contraddittorio non e' un dibattito.",
  },
  {
    key: "orchestrator.subagent_fanout_max_parallel",
    label: "Fan-out per run",
    help: "Quanti sub-agenti dello stesso run girano davvero insieme: limita il tempo di parete, non quanti ne vengono convocati.",
  },
];

// ── Derivazione (logica pura) ────────────────────────────────────────────────

/** Salva un setting decidendo sull'esito STRUTTURATO (regola M).
 *
 *  update_setting risponde HTTP 200 anche quando la UPDATE fallisce
 *  (admin-service/src/settings.rs: ramo Err -> {"status":"error"}), quindi
 *  fetchJson non solleva: senza questo controllo un errore del DB apparirebbe
 *  come un salvataggio riuscito. `created` non e' un successo qui: significa che
 *  la chiave non esisteva e il backend l'ha creata in categoria 'custom', fuori
 *  dalle categorie che questa pagina legge — una chiave fantasma che il
 *  resolver leggerebbe e la UI non mostrerebbe piu'. I controlli sulle chiavi
 *  assenti sono gia' disabilitati: questo e' il presidio di ultima istanza. */
async function saveSetting(key: string, value: string): Promise<void> {
  const res = await updateAdminSetting(key, value);
  if (res.status !== "ok") {
    throw new Error(`Salvataggio di ${key} non riuscito (esito: ${res.status})`);
  }
}

type ProfileValues = Record<ProfileField, number | null>;

interface ProfileRow {
  cls: ComplexityClass;
  key: string;
  /** false = chiave assente dalla categoria orchestrator (mig 0602 non applicata). */
  present: boolean;
  /** true = il valore salvato non e' un oggetto JSON: il resolver non lo puo' leggere. */
  malformed: boolean;
  values: ProfileValues;
}

/** null = campo assente o non interpretabile come conteggio (intero >= 0). */
function readCount(raw: unknown): number | null {
  return typeof raw === "number" && Number.isInteger(raw) && raw >= 0 ? raw : null;
}

function parseProfile(cls: ComplexityClass, entry: AdminSettingEntry | undefined): ProfileRow {
  const key = `orchestrator.sizing_profile_${cls}`;
  const empty: ProfileValues = {
    council_figures: null,
    reviewers: null,
    providers: null,
    advocates: null,
  };
  if (!entry) return { cls, key, present: false, malformed: false, values: empty };
  let parsed: unknown;
  try {
    parsed = JSON.parse(entry.value);
  } catch {
    return { cls, key, present: true, malformed: true, values: empty };
  }
  if (typeof parsed !== "object" || parsed === null || Array.isArray(parsed)) {
    return { cls, key, present: true, malformed: true, values: empty };
  }
  const obj = parsed as Record<string, unknown>;
  return {
    cls,
    key,
    present: true,
    malformed: false,
    values: {
      council_figures: readCount(obj.council_figures),
      reviewers: readCount(obj.reviewers),
      providers: readCount(obj.providers),
      advocates: readCount(obj.advocates),
    },
  };
}

/** Forma canonica del valore salvato: le 4 chiavi nell'ordine della mig 0602. */
function serializeProfile(values: Record<ProfileField, number>): string {
  return JSON.stringify({
    council_figures: values.council_figures,
    reviewers: values.reviewers,
    providers: values.providers,
    advocates: values.advocates,
  });
}

type ProfileDraft = Record<ProfileField, string>;

function draftFromRow(row: ProfileRow): ProfileDraft {
  return {
    council_figures: row.values.council_figures?.toString() ?? "",
    reviewers: row.values.reviewers?.toString() ?? "",
    providers: row.values.providers?.toString() ?? "",
    advocates: row.values.advocates?.toString() ?? "",
  };
}

/** null = la riga non e' salvabile (campo vuoto o non intero >= 0). */
function parseDraft(draft: ProfileDraft): Record<ProfileField, number> | null {
  const out: Partial<Record<ProfileField, number>> = {};
  for (const col of COLUMNS) {
    const raw = draft[col.field].trim();
    if (raw === "") return null;
    const n = Number(raw);
    if (!Number.isInteger(n) || n < 0) return null;
    out[col.field] = n;
  }
  return out as Record<ProfileField, number>;
}

function draftDiffers(row: ProfileRow, draft: ProfileDraft): boolean {
  const base = draftFromRow(row);
  return COLUMNS.some((c) => base[c.field] !== draft[c.field]);
}

interface PriorityState {
  /** Ordine mostrato: panel canonici del CSV, poi quelli mancanti accodati. */
  order: PanelId[];
  /** Panel assenti dal CSV salvato (il resolver li accoda in coda). */
  appended: PanelId[];
  /** Token non canonici presenti nel CSV: il resolver li ignora. */
  unknown: string[];
}

function isPanelId(token: string): token is PanelId {
  return (PANEL_ORDER_DEFAULT as readonly string[]).includes(token);
}

function parsePriority(raw: string | undefined): PriorityState {
  const tokens = (raw ?? "")
    .split(",")
    .map((t) => t.trim())
    .filter(Boolean);
  const order: PanelId[] = [];
  const unknown: string[] = [];
  for (const t of tokens) {
    if (isPanelId(t)) {
      if (!order.includes(t)) order.push(t);
    } else {
      unknown.push(t);
    }
  }
  const appended = PANEL_ORDER_DEFAULT.filter((p) => !order.includes(p));
  return { order: [...order, ...appended], appended: [...appended], unknown };
}

// ── Pagina ───────────────────────────────────────────────────────────────────

export default function OrchestrationSizingPage() {
  const tc = useThemeColors();

  const orchestrator = useListData<AdminSettingEntry>(
    useCallback(() => listAdminSettingsByCategory("orchestrator").then((r) => r.settings), []),
  );
  // I vincoli VERI del resolver (costo e tempo del run) stanno in categoria
  // 'agent', non 'orchestrator': senza questa seconda lettura la pagina
  // mostrerebbe la quota percentuale di un budget invisibile.
  const agent = useListData<AdminSettingEntry>(
    useCallback(() => listAdminSettingsByCategory("agent").then((r) => r.settings), []),
  );

  const byKey = useMemo(() => {
    const m = new Map<string, AdminSettingEntry>();
    for (const s of [...orchestrator.data, ...agent.data]) m.set(s.key, s);
    return m;
  }, [orchestrator.data, agent.data]);

  const reloadOrchestrator = orchestrator.reload;
  const reloadAgent = agent.reload;
  const reloadAll = useCallback(() => {
    void reloadOrchestrator();
    void reloadAgent();
  }, [reloadOrchestrator, reloadAgent]);

  const loading = orchestrator.loading || agent.loading;
  const loadError = orchestrator.error ?? agent.error;

  // ── Sezione A: interruttore del dimensionamento ──
  const sizingEntry = byKey.get(SIZING_ENABLED_KEY);
  const sizingEnabled = sizingEntry?.value === "true";
  const [toggleBusy, setToggleBusy] = useState(false);
  const [toggleError, setToggleError] = useState<string | null>(null);

  const toggleSizing = async () => {
    setToggleBusy(true);
    setToggleError(null);
    try {
      await saveSetting(SIZING_ENABLED_KEY, sizingEnabled ? "false" : "true");
      await reloadOrchestrator();
    } catch (e) {
      setToggleError(e instanceof Error ? e.message : "Errore salvataggio");
    } finally {
      setToggleBusy(false);
    }
  };

  // ── Sezione B: matrice dei profili ──
  const rows = useMemo(
    () =>
      COMPLEXITY_CLASSES.map((cls) =>
        parseProfile(cls, byKey.get(`orchestrator.sizing_profile_${cls}`)),
      ),
    [byKey],
  );

  const [drafts, setDrafts] = useState<Partial<Record<ComplexityClass, ProfileDraft>>>({});
  const [rowBusy, setRowBusy] = useState<ComplexityClass | null>(null);
  const [rowError, setRowError] = useState<string | null>(null);

  const editCell = (row: ProfileRow, field: ProfileField, value: string) => {
    setDrafts((d) => {
      const current = d[row.cls] ?? draftFromRow(row);
      return { ...d, [row.cls]: { ...current, [field]: value } };
    });
  };

  const resetRow = (cls: ComplexityClass) => {
    setDrafts((d) => {
      const next = { ...d };
      delete next[cls];
      return next;
    });
  };

  const saveRow = async (row: ProfileRow) => {
    const draft = drafts[row.cls];
    if (!draft) return;
    const values = parseDraft(draft);
    if (!values) return;
    setRowBusy(row.cls);
    setRowError(null);
    try {
      await saveSetting(row.key, serializeProfile(values));
      resetRow(row.cls);
      await reloadOrchestrator();
    } catch (e) {
      setRowError(e instanceof Error ? e.message : "Errore salvataggio profilo");
    } finally {
      setRowBusy(null);
    }
  };

  // ── Sezione D: ordine di degrado ──
  const priority = useMemo(() => parsePriority(byKey.get(PRIORITY_KEY)?.value), [byKey]);
  const priorityPresent = byKey.has(PRIORITY_KEY);
  const [priorityDraft, setPriorityDraft] = useState<PanelId[] | null>(null);
  const [priorityBusy, setPriorityBusy] = useState(false);
  const [priorityError, setPriorityError] = useState<string | null>(null);
  const effPriority = priorityDraft ?? priority.order;

  const movePanel = (index: number, delta: number) => {
    const target = index + delta;
    if (target < 0 || target >= effPriority.length) return;
    const next = [...effPriority];
    const [moved] = next.splice(index, 1);
    next.splice(target, 0, moved);
    setPriorityDraft(next);
  };

  const savePriority = async () => {
    setPriorityBusy(true);
    setPriorityError(null);
    try {
      await saveSetting(PRIORITY_KEY, effPriority.join(","));
      setPriorityDraft(null);
      await reloadOrchestrator();
    } catch (e) {
      setPriorityError(e instanceof Error ? e.message : "Errore salvataggio ordine");
    } finally {
      setPriorityBusy(false);
    }
  };

  return (
    <div style={{ display: "flex", flexDirection: "column", gap: 28, minWidth: 0 }}>
      <AdminPageHeader
        title="Dimensionamento dell'orchestrazione"
        description="Quante figure, revisori, provider e avvocati convocare per un task, e cosa sacrificare per primo quando il budget non basta. Qui si governa la DECISIONE; i tetti di sicurezza restano nelle impostazioni orchestrator."
        action={
          <button type="button" onClick={reloadAll} style={btnStyle(tc, "ghost")}>
            Ricarica
          </button>
        }
      />

      {/* ── A. Interruttore ─────────────────────────────────────────────────── */}
      <SectionShell
        tc={tc}
        title="Chi decide quante figure convocare"
        subtitle="L'interruttore sceglie tra due regimi diversi, non tra due velocita'."
        loading={loading}
        error={loadError}
      >
        {!sizingEntry ? (
          <MissingKeyNotice tc={tc} settingKey={SIZING_ENABLED_KEY} />
        ) : (
          <div style={{ display: "flex", flexDirection: "column", gap: 12 }}>
            <div style={{ display: "flex", alignItems: "center", gap: 12, flexWrap: "wrap" }}>
              <Badge
                tc={tc}
                tone={sizingEnabled ? "ok" : "muted"}
                label={sizingEnabled ? "Dimensionamento attivo" : "Dimensionamento disattivo (legacy)"}
              />
              <button
                type="button"
                onClick={() => void toggleSizing()}
                disabled={toggleBusy}
                style={btnStyle(tc, sizingEnabled ? "danger" : "primary", toggleBusy)}
              >
                {toggleBusy ? "Salvataggio…" : sizingEnabled ? "Torna al comportamento legacy" : "Attiva il dimensionamento"}
              </button>
              {toggleError ? <span style={{ fontSize: 12, color: tc.error }}>{toggleError}</span> : null}
            </div>
            <ul style={{ margin: 0, paddingLeft: 18, fontSize: 12, color: tc.textMuted, lineHeight: 1.7 }}>
              <li>
                <strong style={{ color: tc.text }}>Spento (comportamento legacy)</strong>: valgono solo i cap
                fissi storici. Ogni task, banale o architetturale, convoca lo stesso numero di figure — quello
                scritto nei tetti qui sotto. Il problema non ha voce in capitolo.
              </li>
              <li>
                <strong style={{ color: tc.text }}>Acceso</strong>: la classe di complessita' dichiarata dal
                classificatore sceglie un profilo (matrice qui sotto), il budget residuo di costo e tempo lo
                stringe se non basta, e i tetti restano come ultimo clamp. Il piano di ogni run e' osservabile
                nel meta-step <code style={codeStyle(tc)}>orchestration_plan</code>, col campo{" "}
                <code style={codeStyle(tc)}>sized_by</code> che dichiara CHI ha deciso: la complessita', il
                budget di costo, quello di tempo o un tetto.
              </li>
            </ul>
          </div>
        )}
      </SectionShell>

      {/* ── B. Matrice ──────────────────────────────────────────────────────── */}
      <SectionShell
        tc={tc}
        title="Profili per classe di complessita'"
        subtitle="La DOMANDA: quanti sub-agenti merita un task di questa classe, budget permettendo. Ogni riga si salva da sola."
        loading={loading}
        error={loadError}
      >
        <div style={{ display: "flex", flexDirection: "column", gap: 10 }}>
          {rowError ? <div style={{ fontSize: 12, color: tc.error }}>{rowError}</div> : null}
          <div style={tableWrapStyle(tc)}>
            <table style={{ width: "100%", borderCollapse: "collapse", background: tc.bgCard }}>
              <thead>
                <tr>
                  <th style={{ ...thStyle(tc), minWidth: 190 }}>Classe</th>
                  {COLUMNS.map((col) => {
                    const panelOn = byKey.get(col.enabledKey)?.value === "true";
                    return (
                      <th key={col.field} style={{ ...thStyle(tc), textAlign: "center", minWidth: 108 }}>
                        <div style={{ display: "flex", flexDirection: "column", alignItems: "center", gap: 3 }}>
                          <span title={col.help}>{col.label}</span>
                          {!panelOn ? <Badge tc={tc} tone="warn" label="panel spento" /> : null}
                        </div>
                      </th>
                    );
                  })}
                  <th style={{ ...thStyle(tc), minWidth: 150 }}>Azioni</th>
                </tr>
              </thead>
              <tbody>
                {rows.map((row) => {
                  const draft = drafts[row.cls] ?? draftFromRow(row);
                  const dirty = drafts[row.cls] !== undefined && draftDiffers(row, draft);
                  const valid = parseDraft(draft) !== null;
                  const busy = rowBusy === row.cls;
                  return (
                    <tr key={row.cls}>
                      <td style={tdStyle(tc)}>
                        <div style={{ display: "flex", flexDirection: "column", gap: 3 }}>
                          <span style={{ fontWeight: 600 }}>
                            {CLASS_LABEL[row.cls]}{" "}
                            <code style={codeStyle(tc)}>{row.cls}</code>
                          </span>
                          <span style={{ fontSize: 11, color: tc.textMuted, maxWidth: 340 }}>
                            {CLASS_HELP[row.cls]}
                          </span>
                          {!row.present ? <Badge tc={tc} tone="warn" label="chiave assente" /> : null}
                          {row.malformed ? (
                            <Badge tc={tc} tone="warn" label="JSON illeggibile: il resolver non lo usa" />
                          ) : null}
                        </div>
                      </td>
                      {COLUMNS.map((col) => (
                        <td key={col.field} style={{ ...tdStyle(tc), textAlign: "center" }}>
                          <input
                            type="number"
                            min={0}
                            step={1}
                            inputMode="numeric"
                            aria-label={`${col.label} — complessita' ${row.cls}`}
                            value={draft[col.field]}
                            disabled={!row.present || busy}
                            onChange={(e) => editCell(row, col.field, e.target.value)}
                            style={numberInputStyle(tc, !row.present || busy)}
                          />
                        </td>
                      ))}
                      <td style={{ ...tdStyle(tc), whiteSpace: "nowrap" }}>
                        <div style={{ display: "flex", gap: 6 }}>
                          <button
                            type="button"
                            onClick={() => void saveRow(row)}
                            disabled={!dirty || !valid || busy}
                            style={btnStyle(tc, "primary", !dirty || !valid || busy)}
                            title={!valid ? "Ogni cella deve contenere un intero >= 0" : undefined}
                          >
                            {busy ? "Salvataggio…" : "Salva riga"}
                          </button>
                          {dirty ? (
                            <button
                              type="button"
                              onClick={() => resetRow(row.cls)}
                              disabled={busy}
                              style={btnStyle(tc, "ghost", busy)}
                            >
                              Annulla
                            </button>
                          ) : null}
                        </div>
                      </td>
                    </tr>
                  );
                })}
              </tbody>
            </table>
          </div>

          <div style={{ display: "flex", flexDirection: "column", gap: 6 }}>
            {COLUMNS.map((col) => (
              <div key={col.field} style={{ fontSize: 11, color: tc.textMuted }}>
                <strong style={{ color: tc.text }}>{col.label}</strong> — {col.help}
              </div>
            ))}
          </div>

          <NoteBox tc={tc} title="Numeri che oggi non hanno effetto (e perche')">
            <ul style={{ margin: 0, paddingLeft: 18, lineHeight: 1.7 }}>
              <li>
                <strong style={{ color: tc.text }}>Riga low</strong>: i task a complessita' bassa non arrivano
                mai ai panel — il gate a monte decide che non meritano deliberazione. Il profilo esiste per
                completezza e per un eventuale gate futuro piu' permissivo: cambiarlo oggi non cambia nessun run.
              </li>
              <li>
                <strong style={{ color: tc.text }}>Colonna avvocati</strong>: il dibattito non si tiene su ogni
                task ad alta complessita'. Serve che il consiglio dichiari una decisione architetturale contesa
                (<code style={codeStyle(tc)}>contested_decision</code>: piu' strade difendibili, nessuna
                ovviamente superiore). Senza quella dichiarazione questo numero resta inerte, per quanto alto.
                Sotto 2 avvocati il dibattito non si tiene comunque: senza contraddittorio non e' un dibattito.
              </li>
              <li>
                <strong style={{ color: tc.text }}>Colonne con &quot;panel spento&quot;</strong>: il panel
                corrispondente e' disattivato nelle impostazioni orchestrator. Il numero e' salvato e valido, ma
                nessuno lo legge finche' quel panel resta spento.
              </li>
              <li>
                Un panel che scenderebbe sotto il proprio quorum minimo viene portato a{" "}
                <strong style={{ color: tc.text }}>zero</strong>, non a uno: un panel convocato monco produce un
                esito inconcludente garantito, cioe' spesa senza decisione.
              </li>
            </ul>
          </NoteBox>
        </div>
      </SectionShell>

      {/* ── C. Budget ───────────────────────────────────────────────────────── */}
      <SectionShell
        tc={tc}
        title="Budget: il vincolo che stringe davvero"
        subtitle="L'OFFERTA. Il profilo dice quanto vorresti; questi numeri dicono quanto ti puoi permettere. Vince il piu' stretto tra costo e tempo, e il piano dichiara quale dei due ha deciso."
        loading={loading}
        error={loadError}
      >
        <div style={{ display: "flex", flexDirection: "column", gap: 12 }}>
          <SettingNumberEditor
            tc={tc}
            settingKey="orchestrator.sizing.budget_share_pct"
            entry={byKey.get("orchestrator.sizing.budget_share_pct")}
            label="Quota per i panel"
            unit="%"
            help="Percentuale del budget di costo RESIDUO del run spendibile nei panel. Il resto resta al run principale, che deve poter finire: e' la fetta dei consulenti, non il portafoglio intero."
            max={100}
            onSaved={reloadOrchestrator}
          />
          <SettingNumberEditor
            tc={tc}
            settingKey="agent.run_cost_budget_usd"
            entry={byKey.get("agent.run_cost_budget_usd")}
            label="Budget di costo del run"
            unit="USD"
            help="Tetto di spesa dell'intero run. La quota qui sopra si calcola su quanto ne resta al momento del piano. 0 = nessun limite di costo (il dimensionamento non potra' stringere sui soldi)."
            allowDecimals
            onSaved={reloadAgent}
          />
          <SettingNumberEditor
            tc={tc}
            settingKey="agent.run_time_budget_s"
            entry={byKey.get("agent.run_time_budget_s")}
            label="Budget di tempo del run"
            unit="secondi"
            help="Deadline di parete dell'intero run (sopravvive ai resume). Il tempo residuo limita quanti sub-run stanno in piedi in parallelo. 0 = nessuna deadline (il dimensionamento non potra' stringere sul tempo)."
            onSaved={reloadAgent}
          />
          <NoteBox tc={tc} title="Suggerimenti informativi (non vincolano nulla)">
            <div style={{ display: "flex", gap: 8, flexWrap: "wrap", marginBottom: 6 }}>
              <ConfigChip
                tc={tc}
                label="Costo suggerito"
                value={byKey.get("orchestrator.sizing_budget_cost_usd_default")?.value ?? "—"}
              />
              <ConfigChip
                tc={tc}
                label="Tempo suggerito"
                value={byKey.get("orchestrator.sizing_budget_time_s_default")?.value ?? "—"}
              />
            </div>
            Queste due chiavi (<code style={codeStyle(tc)}>sizing_budget_cost_usd_default</code>,{" "}
            <code style={codeStyle(tc)}>sizing_budget_time_s_default</code>) sono promemoria di dimensionamento
            per questa pagina: nessun run le legge. I vincoli veri sono i due editabili qui sopra — se sono
            entrambi a 0, il dimensionamento non ha nulla su cui stringere e il piano dipendera' solo dalla
            classe di complessita' e dai tetti.
          </NoteBox>
        </div>
      </SectionShell>

      {/* ── D. Ordine di degrado ────────────────────────────────────────────── */}
      <SectionShell
        tc={tc}
        title="Ordine di degrado"
        subtitle="Quando il budget non basta per tutto, qualcosa deve cadere. Questo e' l'ordine in cui si cede: l'ULTIMO della lista si sacrifica per primo."
        loading={loading}
        error={loadError}
      >
        {!priorityPresent ? (
          <MissingKeyNotice tc={tc} settingKey={PRIORITY_KEY} />
        ) : (
          <div style={{ display: "flex", flexDirection: "column", gap: 10 }}>
            <ol style={{ margin: 0, padding: 0, listStyle: "none", display: "flex", flexDirection: "column", gap: 6 }}>
              {effPriority.map((panel, index) => (
                <li
                  key={panel}
                  style={{
                    display: "flex",
                    alignItems: "center",
                    gap: 10,
                    padding: "8px 10px",
                    border: `1px solid ${tc.border}`,
                    borderRadius: 8,
                    background: tc.bgCard,
                  }}
                >
                  <span style={{ fontSize: 12, color: tc.textMuted, width: 18 }}>{index + 1}.</span>
                  <span style={{ fontSize: 13, color: tc.text, fontWeight: 600, minWidth: 150 }}>
                    {PANEL_LABEL[panel]}
                  </span>
                  <code style={codeStyle(tc)}>{panel}</code>
                  <span style={{ fontSize: 11, color: tc.textMuted, flex: 1 }}>
                    {index === 0
                      ? "Il piu' protetto: cede solo se non resta altro da tagliare."
                      : index === effPriority.length - 1
                        ? "Il primo a cadere quando il budget non basta."
                        : ""}
                  </span>
                  {priority.appended.includes(panel) ? (
                    <Badge tc={tc} tone="warn" label="assente dal valore salvato" />
                  ) : null}
                  <div style={{ display: "flex", gap: 4 }}>
                    <button
                      type="button"
                      onClick={() => movePanel(index, -1)}
                      disabled={index === 0 || priorityBusy}
                      aria-label={`Alza la priorita' di ${PANEL_LABEL[panel]}`}
                      style={btnStyle(tc, "ghost", index === 0 || priorityBusy)}
                    >
                      ↑
                    </button>
                    <button
                      type="button"
                      onClick={() => movePanel(index, 1)}
                      disabled={index === effPriority.length - 1 || priorityBusy}
                      aria-label={`Abbassa la priorita' di ${PANEL_LABEL[panel]}`}
                      style={btnStyle(tc, "ghost", index === effPriority.length - 1 || priorityBusy)}
                    >
                      ↓
                    </button>
                  </div>
                </li>
              ))}
            </ol>

            {priority.appended.length > 0 ? (
              <div style={{ fontSize: 11, color: tc.textMuted }}>
                I panel marcati &quot;assente dal valore salvato&quot; non compaiono nel CSV in DB: il resolver
                li accoda in fondo nell'ordine di default, cioe' li sacrifica per primi. Salvando qui, l'ordine
                mostrato diventa quello scritto.
              </div>
            ) : null}
            {priority.unknown.length > 0 ? (
              <div style={{ fontSize: 11, color: tc.error }}>
                Il valore salvato contiene token che il resolver ignora ({priority.unknown.join(", ")}): non sono
                panel esistenti. Salvando da qui vengono rimossi.
              </div>
            ) : null}

            <div style={{ display: "flex", alignItems: "center", gap: 12, flexWrap: "wrap" }}>
              <button
                type="button"
                onClick={() => void savePriority()}
                disabled={priorityDraft === null || priorityBusy}
                style={btnStyle(tc, "primary", priorityDraft === null || priorityBusy)}
              >
                {priorityBusy ? "Salvataggio…" : "Salva ordine"}
              </button>
              {priorityDraft !== null ? (
                <button
                  type="button"
                  onClick={() => setPriorityDraft(null)}
                  disabled={priorityBusy}
                  style={btnStyle(tc, "ghost", priorityBusy)}
                >
                  Annulla
                </button>
              ) : null}
              <span style={{ fontSize: 11, color: tc.textMuted }}>
                Valore salvato: <code style={codeStyle(tc)}>{byKey.get(PRIORITY_KEY)?.value || "(vuoto)"}</code>
              </span>
              {priorityError ? <span style={{ fontSize: 12, color: tc.error }}>{priorityError}</span> : null}
            </div>
          </div>
        )}
      </SectionShell>

      {/* ── E. Stima per sub-run ────────────────────────────────────────────── */}
      <SectionShell
        tc={tc}
        title="Quanto costa un consigliere"
        subtitle="Il prezzo di un panel va stimato PRIMA di convocarlo: senza un costo unitario atteso, il budget non puo' dire quanti sub-run ci stanno."
        loading={loading}
        error={loadError}
      >
        <div style={{ display: "flex", flexDirection: "column", gap: 12 }}>
          <SettingNumberEditor
            tc={tc}
            settingKey="orchestrator.sizing.est_subrun_tokens"
            entry={byKey.get("orchestrator.sizing.est_subrun_tokens")}
            label="Token attesi per sub-run"
            unit="token"
            help="Token totali attesi di un sub-run advisory. Moltiplicati per il prezzo di listino del modello risolto via tier danno il costo unitario: e' il numero che il budget divide per sapere quante figure ci stanno."
            onSaved={reloadOrchestrator}
          />
          <SettingNumberEditor
            tc={tc}
            settingKey="orchestrator.sizing.est_subrun_duration_s"
            entry={byKey.get("orchestrator.sizing.est_subrun_duration_s")}
            label="Durata attesa per sub-run"
            unit="secondi"
            help="Durata attesa di un sub-run advisory. Col tempo residuo del run e il parallelismo del fan-out dice quanti sub-run stanno nella deadline."
            onSaved={reloadOrchestrator}
          />
          <div style={{ fontSize: 11, color: tc.textMuted }}>
            Sono stime, non misure: se sbagliano per eccesso il sistema convoca meno figure del necessario, se
            sbagliano per difetto sfora il budget che credeva di rispettare. Vanno riallineate alla mediana reale
            dei sub-run osservati.
          </div>
        </div>
      </SectionShell>

      {/* ── F. Backstop ─────────────────────────────────────────────────────── */}
      <SectionShell
        tc={tc}
        title="Tetti di sicurezza (sola lettura)"
        subtitle="Erano la decisione, ora sono solo il limite: nessun piano puo' superarli, ma non convocano piu' nessuno. Si modificano dalle impostazioni orchestrator."
        loading={loading}
        error={loadError}
      >
        <div style={{ display: "flex", flexDirection: "column", gap: 10 }}>
          <div style={tableWrapStyle(tc)}>
            <table style={{ width: "100%", borderCollapse: "collapse", background: tc.bgCard }}>
              <thead>
                <tr>
                  <th style={thStyle(tc)}>Tetto</th>
                  <th style={{ ...thStyle(tc), textAlign: "center" }}>Valore</th>
                  <th style={thStyle(tc)}>Chiave</th>
                  <th style={thStyle(tc)}>A cosa serve</th>
                </tr>
              </thead>
              <tbody>
                {BACKSTOPS.map((b) => {
                  const entry = byKey.get(b.key);
                  return (
                    <tr key={b.key}>
                      <td style={{ ...tdStyle(tc), fontWeight: 600, whiteSpace: "nowrap" }}>{b.label}</td>
                      <td style={{ ...tdStyle(tc), textAlign: "center", fontWeight: 600 }}>
                        {entry ? entry.value : <Badge tc={tc} tone="warn" label="assente" />}
                      </td>
                      <td style={tdStyle(tc)}>
                        <code style={codeStyle(tc)}>{b.key}</code>
                      </td>
                      <td style={{ ...tdStyle(tc), fontSize: 11, color: tc.textMuted }}>{b.help}</td>
                    </tr>
                  );
                })}
              </tbody>
            </table>
          </div>
          <div style={{ fontSize: 12, color: tc.textMuted }}>
            Modifica dei tetti e degli altri parametri dei panel:{" "}
            <a href={SETTINGS_ORCHESTRATOR_HREF} style={{ color: tc.accent }}>
              Impostazioni orchestrator
            </a>
          </div>
        </div>
      </SectionShell>
    </div>
  );
}

// ── Editor di un singolo setting numerico ────────────────────────────────────

/** Editor di una chiave numerica: bozza locale, validazione esplicita, salvataggio
 *  sul punto unico. Su chiave assente NON offre la modifica: il backend creerebbe
 *  la chiave in categoria 'custom' (fuori da questa pagina) invece di segnalare
 *  che la migrazione non e' stata applicata. */
function SettingNumberEditor({
  tc,
  settingKey,
  entry,
  label,
  help,
  unit,
  max,
  allowDecimals = false,
  onSaved,
}: {
  tc: ThemeColors;
  settingKey: string;
  entry: AdminSettingEntry | undefined;
  label: string;
  help: string;
  unit: string;
  max?: number;
  allowDecimals?: boolean;
  onSaved: () => Promise<void> | void;
}) {
  const [draft, setDraft] = useState<string | null>(null);
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);

  if (!entry) {
    return (
      <div style={{ display: "flex", flexDirection: "column", gap: 4 }}>
        <span style={{ fontSize: 13, fontWeight: 600, color: tc.text }}>{label}</span>
        <MissingKeyNotice tc={tc} settingKey={settingKey} />
      </div>
    );
  }

  const saved = entry.value;
  const value = draft ?? saved;
  const dirty = draft !== null && draft !== saved;
  const parsed = Number(value.trim());
  const valid =
    value.trim() !== "" &&
    Number.isFinite(parsed) &&
    parsed >= 0 &&
    (max === undefined || parsed <= max) &&
    (allowDecimals || Number.isInteger(parsed));

  const save = async () => {
    if (!dirty || !valid) return;
    setBusy(true);
    setError(null);
    try {
      await saveSetting(settingKey, value.trim());
      setDraft(null);
      await onSaved();
    } catch (e) {
      setError(e instanceof Error ? e.message : "Errore salvataggio");
    } finally {
      setBusy(false);
    }
  };

  const bound =
    max !== undefined
      ? `tra 0 e ${max}`
      : allowDecimals
        ? "maggiore o uguale a 0"
        : "intero maggiore o uguale a 0";

  return (
    <div
      style={{
        display: "flex",
        flexDirection: "column",
        gap: 6,
        padding: "10px 12px",
        border: `1px solid ${tc.border}`,
        borderRadius: 8,
        background: tc.bgCard,
      }}
    >
      <div style={{ display: "flex", alignItems: "center", gap: 10, flexWrap: "wrap" }}>
        <span style={{ fontSize: 13, fontWeight: 600, color: tc.text, minWidth: 190 }}>{label}</span>
        <input
          type="number"
          min={0}
          max={max}
          step={allowDecimals ? "any" : 1}
          inputMode="decimal"
          aria-label={label}
          value={value}
          disabled={busy}
          onChange={(e) => setDraft(e.target.value)}
          style={{ ...numberInputStyle(tc, busy), width: 120 }}
        />
        <span style={{ fontSize: 12, color: tc.textMuted, minWidth: 56 }}>{unit}</span>
        <button
          type="button"
          onClick={() => void save()}
          disabled={!dirty || !valid || busy}
          style={btnStyle(tc, "primary", !dirty || !valid || busy)}
        >
          {busy ? "Salvataggio…" : "Salva"}
        </button>
        {dirty ? (
          <button type="button" onClick={() => setDraft(null)} disabled={busy} style={btnStyle(tc, "ghost", busy)}>
            Annulla
          </button>
        ) : null}
        <code style={{ ...codeStyle(tc), marginLeft: "auto" }}>{settingKey}</code>
      </div>
      <span style={{ fontSize: 11, color: tc.textMuted }}>{help}</span>
      {dirty && !valid ? (
        <span style={{ fontSize: 11, color: tc.error }}>Valore non valido: atteso un numero {bound}.</span>
      ) : null}
      {error ? <span style={{ fontSize: 11, color: tc.error }}>{error}</span> : null}
    </div>
  );
}

// ── Helper di presentazione ──────────────────────────────────────────────────

function MissingKeyNotice({ tc, settingKey }: { tc: ThemeColors; settingKey: string }) {
  return (
    <div
      style={{
        fontSize: 12,
        color: tc.textMuted,
        padding: "8px 10px",
        border: `1px solid ${tc.border}`,
        borderRadius: 8,
        background: tc.bgCard,
      }}
    >
      <Badge tc={tc} tone="warn" label="chiave assente" />{" "}
      <code style={codeStyle(tc)}>{settingKey}</code> non esiste nel DB: la migrazione che la introduce non e'
      stata applicata. La modifica e' disabilitata di proposito — scrivere ora creerebbe la chiave in una
      categoria diversa da quella attesa, invisibile a questa pagina.
    </div>
  );
}

function SectionShell({
  tc,
  title,
  subtitle,
  loading,
  error,
  children,
}: {
  tc: ThemeColors;
  title: string;
  subtitle: string;
  loading: boolean;
  error: string | null;
  children: ReactNode;
}) {
  return (
    <section style={{ display: "flex", flexDirection: "column", gap: 12, minWidth: 0 }}>
      <div style={{ minWidth: 0 }}>
        <h3 style={{ fontSize: 15, fontWeight: 700, margin: "0 0 2px", color: tc.text }}>{title}</h3>
        <p style={{ fontSize: 12, color: tc.textMuted, margin: 0 }}>{subtitle}</p>
      </div>
      {loading ? (
        <div style={{ fontSize: 13, color: tc.textMuted, padding: "8px 0" }}>Caricamento…</div>
      ) : error ? (
        <div
          style={{
            fontSize: 13,
            color: tc.error,
            padding: "10px 12px",
            borderRadius: 8,
            border: `1px solid ${tc.border}`,
            background: tc.bgCard,
          }}
        >
          Errore: {error}
        </div>
      ) : (
        children
      )}
    </section>
  );
}

function NoteBox({ tc, title, children }: { tc: ThemeColors; title: string; children: ReactNode }) {
  return (
    <div
      style={{
        fontSize: 11,
        color: tc.textMuted,
        padding: "10px 12px",
        border: `1px solid ${tc.border}`,
        borderRadius: 8,
        background: tc.bgCard,
      }}
    >
      <div style={{ fontSize: 12, fontWeight: 600, color: tc.text, marginBottom: 6 }}>{title}</div>
      {children}
    </div>
  );
}

function tableWrapStyle(tc: ThemeColors): CSSProperties {
  return {
    display: "block",
    overflowX: "auto",
    border: `1px solid ${tc.border}`,
    borderRadius: 8,
    minWidth: 0,
  };
}

function thStyle(tc: ThemeColors): CSSProperties {
  return {
    textAlign: "left",
    padding: "8px 10px",
    fontSize: 11,
    fontWeight: 600,
    textTransform: "uppercase",
    letterSpacing: "0.04em",
    color: tc.textMuted,
    borderBottom: `1px solid ${tc.border}`,
    whiteSpace: "nowrap",
  };
}

function tdStyle(tc: ThemeColors): CSSProperties {
  return {
    padding: "8px 10px",
    fontSize: 12,
    color: tc.text,
    borderBottom: `1px solid ${tc.border}`,
    verticalAlign: "top",
  };
}

function codeStyle(tc: ThemeColors): CSSProperties {
  return {
    fontFamily: "var(--font-mono)",
    fontSize: 11,
    color: tc.textMuted,
    whiteSpace: "nowrap",
  };
}

function numberInputStyle(tc: ThemeColors, disabled: boolean): CSSProperties {
  return {
    width: 72,
    padding: "5px 8px",
    textAlign: "center",
    background: disabled ? tc.bgInput : tc.bgCard,
    color: disabled ? tc.textMuted : tc.text,
    border: `1px solid ${tc.border}`,
    borderRadius: 6,
    fontSize: 12,
    fontFamily: "var(--font-mono)",
    boxSizing: "border-box",
  };
}

function Badge({ tc, label, tone }: { tc: ThemeColors; label: string; tone: "ok" | "warn" | "muted" | "info" }) {
  const palette: Record<string, { bg: string; fg: string }> = {
    ok: { bg: "rgba(74,222,128,0.15)", fg: tc.success },
    warn: { bg: "rgba(248,113,113,0.15)", fg: tc.error },
    info: { bg: tc.accentBg, fg: tc.accent },
    muted: { bg: tc.border, fg: tc.textMuted },
  };
  const { bg, fg } = palette[tone];
  return (
    <span
      style={{
        display: "inline-block",
        flexShrink: 0,
        padding: "2px 8px",
        borderRadius: 6,
        fontSize: 10,
        fontWeight: 600,
        background: bg,
        color: fg,
        whiteSpace: "nowrap",
      }}
    >
      {label}
    </span>
  );
}

function ConfigChip({ tc, label, value }: { tc: ThemeColors; label: string; value: string }) {
  return (
    <span
      style={{
        display: "inline-flex",
        gap: 6,
        alignItems: "baseline",
        padding: "4px 10px",
        borderRadius: 8,
        border: `1px solid ${tc.border}`,
        background: tc.bgCard,
        fontSize: 12,
      }}
    >
      <span style={{ color: tc.textMuted }}>{label}</span>
      <span style={{ color: tc.text, fontWeight: 600 }}>{value}</span>
    </span>
  );
}

function btnStyle(tc: ThemeColors, variant: "primary" | "ghost" | "danger", disabled = false): CSSProperties {
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
  if (variant === "danger") {
    return { ...base, color: tc.error, borderColor: tc.error, background: "transparent" };
  }
  return base;
}
