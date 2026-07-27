//! Il piano: da catalogo + binari del workspace + scostamenti versionati alla
//! lista dei servizi da generare.
//!
//! PURA di proposito: niente DB, niente cargo, niente filesystem. Tutto cio' che
//! le serve arriva come parametro, perche' e' la funzione che i test di
//! mutazione devono poter attaccare senza avere un ambiente addosso (regola O).

use std::collections::{BTreeMap, BTreeSet};

use nexus_service_catalog::CatalogEntry;

use super::overrides::{ExecOverride, Ordine};

/// Da dove viene la regola di esecuzione di un servizio. Finisce nell'indice
/// generato: chi legge un manifest deve poter sapere quale regola l'ha prodotto.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Provenienza {
    /// Binario del workspace: `cargo metadata` dice che esiste e come si chiama.
    WorkspaceBin { package: String, bin: String },
    /// Scostamento dichiarato nel TOML versionato (indice della voce nel file).
    Scostamento { file: String, indice: usize },
}

/// Un servizio risolto, pronto da emettere.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ServizioRisolto {
    pub winsw_id: String,
    pub nome_catalogo: String,
    pub display: String,
    pub descrizione: Option<String>,
    pub executable: String,
    pub arguments: Vec<String>,
    pub working_directory: String,
    pub env: Vec<(String, String)>,
    pub porta: Option<u16>,
    pub provenienza: Provenienza,
}

/// Perche' il piano non e' producibile. Un errore, non un manifest sbagliato.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PlanError {
    /// Voce di catalogo con `winsw_id` ma senza binario nel workspace e senza
    /// scostamento: e' esattamente la forma di `billing-service` dopo la
    /// rimozione del crate, e di qualunque servizio dichiarato e mai costruito.
    BinarioAssente { nome: String, winsw_id: String },
    /// Scostamento che copre un servizio con un package nel workspace: le due
    /// fonti tornerebbero a sovrapporsi.
    ScostamentoSuPackage { nome: String },
    /// Scostamento per un nome che il catalogo non contiene.
    ScostamentoOrfano { nome: String },
    /// Due voci di catalogo con lo stesso `winsw_id`.
    IdCollisione { winsw_id: String, nomi: Vec<String> },
    /// Un id del piano non compare nell'ordine di avvio: e' la forma esatta del
    /// difetto di browser-bridge, che aveva il servizio ma non l'avvio.
    OrdineIncompleto { mancanti: Vec<String> },
    /// L'ordine nomina un id che il piano non contiene.
    OrdineOrfano { estranei: Vec<String> },
    /// Porta richiesta da un argomento o da una variabile, ma non risolvibile.
    PortaNonRisolvibile { nome: String, chiave: String },
    /// Placeholder non riconosciuto: mai sostituito in silenzio.
    PlaceholderIgnoto { nome: String, testo: String },
    /// Il servizio che ospita il DB da cui si legge il catalogo non puo' essere
    /// generato da questo comando: sarebbe la dipendenza circolare.
    ServizioDelDb { nome: String, porta: u16 },
}

impl std::fmt::Display for PlanError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::OrdineIncompleto { .. }
            | Self::OrdineOrfano { .. }
            | Self::PortaNonRisolvibile { .. }
            | Self::PlaceholderIgnoto { .. }
            | Self::ServizioDelDb { .. } => self.fmt_avvio(f),
            _ => self.fmt_identita(f),
        }
    }
}

impl PlanError {
    /// Difetti dell'identita' dei servizi: chi esiste, chi lo dichiara, chi lo
    /// costruisce.
    fn fmt_identita(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::BinarioAssente { nome, winsw_id } => write!(
                f,
                "'{nome}' e' nel catalogo con winsw_id '{winsw_id}' ma il workspace non \
                 produce un binario con quel nome. Il join e' su `name`: la voce di \
                 catalogo deve chiamarsi come il package cargo e come il suo [[bin]]. \
                 Se il servizio non esiste piu', va tolto dal catalogo con una migrazione; \
                 se non l'hai ancora costruito, esegui `cargo build`."
            ),
            Self::ScostamentoSuPackage { nome } => write!(
                f,
                "'{nome}' ha un package nel workspace: il suo avvio si deriva da li', \
                 non da service-exec-overrides.toml. Due fonti per lo stesso fatto \
                 tornerebbero a divergere."
            ),
            Self::ScostamentoOrfano { nome } => write!(
                f,
                "service-exec-overrides.toml dichiara '{nome}', che non e' nel catalogo: \
                 o la voce e' stata rimossa dal DB, o il nome e' scritto male."
            ),
            Self::IdCollisione { winsw_id, nomi } => write!(
                f,
                "winsw_id '{winsw_id}' dichiarato da piu' voci di catalogo ({}): \
                 genererebbero lo stesso manifest sovrascrivendosi.",
                nomi.join(", ")
            ),
            _ => Ok(()),
        }
    }

    /// Difetti di cio' che serve ad AVVIARE il servizio: ordine, porte,
    /// placeholder, e il servizio che ospita il catalogo.
    fn fmt_avvio(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::OrdineIncompleto { mancanti } => write!(
                f,
                "l'ordine di avvio non nomina {}: un servizio con manifest ma senza \
                 posto nell'avvio non verrebbe mai avviato (e' il difetto di \
                 nexus-browser-bridge).",
                mancanti.join(", ")
            ),
            Self::OrdineOrfano { estranei } => write!(
                f,
                "l'ordine di avvio nomina {}, che il piano non contiene.",
                estranei.join(", ")
            ),
            Self::PortaNonRisolvibile { nome, chiave } => write!(
                f,
                "'{nome}' usa la propria porta ma non e' risolvibile (chiave '{chiave}'): \
                 aggiungerla ai settings, mai scriverla a mano nel manifest."
            ),
            Self::PlaceholderIgnoto { nome, testo } => write!(
                f,
                "'{nome}' usa un placeholder non riconosciuto in {testo:?}: ammessi \
                 ${{REPO}} ${{RUNTIME}} ${{NODE}} ${{EXE}} ${{PORT}}."
            ),
            Self::ServizioDelDb { nome, porta } => write!(
                f,
                "'{nome}' ascolta sulla porta {porta}, la stessa di DATABASE_URL: e' il \
                 servizio che ospita il catalogo da cui questo comando legge. Non puo' \
                 dipendere da un manifest generato leggendo se stesso."
            ),
            _ => Ok(()),
        }
    }
}

/// Tutto cio' che il piano deve sapere del mondo, passato esplicitamente.
#[derive(Debug, Clone, Default)]
pub struct Ambiente {
    pub repo_root: String,
    pub runtime_root: String,
    pub bin_dir: String,
    /// Estensione eseguibile della piattaforma (".exe" su Windows, "" altrove).
    pub exe_ext: String,
    pub node: Option<String>,
    pub dotenv: BTreeMap<String, String>,
    /// Porte gia' risolte per nome di servizio (dal catalogo, via DB).
    pub porte: BTreeMap<String, u16>,
    /// Porta di DATABASE_URL: serve a riconoscere il servizio che ospita il
    /// catalogo, MISURANDOLA invece di riconoscerlo dal nome.
    pub porta_db: Option<u16>,
}

/// Costruisce il piano. Raccoglie TUTTI gli errori invece di fermarsi al primo:
/// un insieme parziale di manifest e' la forma sotto cui il difetto originale
/// si e' presentato, e non deve essere un esito possibile.
pub fn plan(
    catalogo: &[CatalogEntry],
    bins: &BTreeMap<String, String>,
    scostamenti: &[ExecOverride],
    ordine: &Ordine,
    amb: &Ambiente,
) -> Result<Vec<ServizioRisolto>, Vec<PlanError>> {
    let mut errori: Vec<PlanError> = Vec::new();
    let mut risolti: Vec<ServizioRisolto> = Vec::new();

    errori.extend(valida_scostamenti(catalogo, bins, scostamenti));
    errori.extend(valida_id_unici(catalogo));

    for e in catalogo.iter() {
        match risolvi_voce(e, bins, scostamenti, amb) {
            Ok(Some(r)) => risolti.push(r),
            Ok(None) => {}
            Err(mut errs) => errori.append(&mut errs),
        }
    }

    errori.extend(valida_ordine(&risolti, ordine));

    if errori.is_empty() {
        Ok(risolti)
    } else {
        Err(errori)
    }
}

/// Risolve una singola voce: `None` se non dichiara un servizio Windows.
fn risolvi_voce(
    e: &CatalogEntry,
    bins: &BTreeMap<String, String>,
    scostamenti: &[ExecOverride],
    amb: &Ambiente,
) -> Result<Option<ServizioRisolto>, Vec<PlanError>> {
    let Some(winsw_id) = e.winsw_id.clone() else {
        return Ok(None); // voce senza servizio Windows: legittima (es. postgres).
    };
    let porta = amb.porte.get(&e.name).copied();

    // Anti-circolarita' MISURATA: non "se si chiama postgres", ma "se ascolta
    // dove ascolta il DB da cui ho letto questo catalogo".
    if let (Some(p), Some(pdb)) = (porta, amb.porta_db) {
        if p == pdb {
            return Err(vec![PlanError::ServizioDelDb {
                nome: e.name.clone(),
                porta: p,
            }]);
        }
    }

    let (executable, arguments, working_directory, env, provenienza) =
        exec_spec(e, bins, scostamenti, porta, amb, &winsw_id)?;
    Ok(Some(ServizioRisolto {
        winsw_id,
        nome_catalogo: e.name.clone(),
        display: display_di(e),
        descrizione: e.description.clone(),
        executable,
        arguments,
        working_directory,
        env,
        porta,
        provenienza,
    }))
}

type Spec = (String, Vec<String>, String, Vec<(String, String)>, Provenienza);

/// Da dove viene l'esecuzione: workspace se il binario esiste, altrimenti lo
/// scostamento versionato. Se nessuno dei due, e' un errore.
fn exec_spec(
    e: &CatalogEntry,
    bins: &BTreeMap<String, String>,
    scostamenti: &[ExecOverride],
    porta: Option<u16>,
    amb: &Ambiente,
    winsw_id: &str,
) -> Result<Spec, Vec<PlanError>> {
    if let Some(bin) = bins.get(&e.name) {
        return Ok((
            format!("{}/{}{}", amb.bin_dir, bin, amb.exe_ext),
            Vec::new(),
            amb.repo_root.clone(),
            Vec::new(),
            Provenienza::WorkspaceBin {
                package: e.name.clone(),
                bin: bin.clone(),
            },
        ));
    }
    match scostamenti.iter().enumerate().find(|(_, s)| s.catalogo == e.name) {
        Some((idx, s)) => {
            let (exe, args, wd, env) = super::overrides::risolvi(s, e, porta, amb)?;
            Ok((
                exe,
                args,
                wd,
                env,
                Provenienza::Scostamento {
                    file: s.file.clone(),
                    indice: idx,
                },
            ))
        }
        None => Err(vec![PlanError::BinarioAssente {
            nome: e.name.clone(),
            winsw_id: winsw_id.to_string(),
        }]),
    }
}

/// Uno scostamento deve riferirsi a una voce esistente e non puo' coprire un
/// binario del workspace: e' il guardiano del confine fra le due fonti.
fn valida_scostamenti(
    catalogo: &[CatalogEntry],
    bins: &BTreeMap<String, String>,
    scostamenti: &[ExecOverride],
) -> Vec<PlanError> {
    let nomi: BTreeSet<&str> = catalogo.iter().map(|e| e.name.as_str()).collect();
    scostamenti
        .iter()
        .filter_map(|s| {
            if !nomi.contains(s.catalogo.as_str()) {
                Some(PlanError::ScostamentoOrfano {
                    nome: s.catalogo.clone(),
                })
            } else if bins.contains_key(&s.catalogo) {
                Some(PlanError::ScostamentoSuPackage {
                    nome: s.catalogo.clone(),
                })
            } else {
                None
            }
        })
        .collect()
}

/// Due voci con lo stesso `winsw_id` scriverebbero lo stesso file.
fn valida_id_unici(catalogo: &[CatalogEntry]) -> Vec<PlanError> {
    let mut per_id: BTreeMap<&str, Vec<String>> = BTreeMap::new();
    for e in catalogo.iter() {
        if let Some(id) = e.winsw_id.as_deref() {
            per_id.entry(id).or_default().push(e.name.clone());
        }
    }
    per_id
        .into_iter()
        .filter(|(_, nomi)| nomi.len() > 1)
        .map(|(id, nomi)| PlanError::IdCollisione {
            winsw_id: id.to_string(),
            nomi,
        })
        .collect()
}

/// L'ordine di avvio deve nominare esattamente il piano: ne' meno (un servizio
/// con manifest che nessuno avvia) ne' piu' (un id che non esiste).
fn valida_ordine(risolti: &[ServizioRisolto], ordine: &Ordine) -> Vec<PlanError> {
    let ids_piano: BTreeSet<&str> = risolti.iter().map(|r| r.winsw_id.as_str()).collect();
    let ids_ordine: BTreeSet<&str> = ordine.avvio.iter().map(|s| s.as_str()).collect();
    let mut out = Vec::new();
    let mancanti: Vec<String> = ids_piano
        .difference(&ids_ordine)
        .map(|s| (*s).to_string())
        .collect();
    if !mancanti.is_empty() {
        out.push(PlanError::OrdineIncompleto { mancanti });
    }
    let estranei: Vec<String> = ids_ordine
        .difference(&ids_piano)
        .map(|s| (*s).to_string())
        .collect();
    if !estranei.is_empty() {
        out.push(PlanError::OrdineOrfano { estranei });
    }
    out
}

fn display_di(e: &CatalogEntry) -> String {
    if e.label.is_empty() {
        e.name.clone()
    } else {
        e.label.clone()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn voce(name: &str, winsw: Option<&str>) -> CatalogEntry {
        serde_json::from_value(serde_json::json!({
            "name": name,
            "label": name,
            "winsw_id": winsw,
        }))
        .expect("voce di catalogo")
    }

    fn amb() -> Ambiente {
        Ambiente {
            repo_root: "R".into(),
            runtime_root: "RT".into(),
            bin_dir: "R/target/debug".into(),
            exe_ext: ".exe".into(),
            node: Some("node.exe".into()),
            ..Default::default()
        }
    }

    fn ordine(ids: &[&str]) -> Ordine {
        Ordine {
            avvio: ids.iter().map(|s| (*s).to_string()).collect(),
            attesa_dopo: BTreeMap::new(),
        }
    }

    #[test]
    fn un_binario_del_workspace_si_deriva_senza_dichiararlo() {
        let cat = vec![voce("mcp-core", Some("nexus-mcp-core"))];
        let bins = BTreeMap::from([("mcp-core".to_string(), "mcp-core".to_string())]);
        let p = plan(&cat, &bins, &[], &ordine(&["nexus-mcp-core"]), &amb()).expect("piano");
        assert_eq!(p.len(), 1);
        assert_eq!(p[0].executable, "R/target/debug/mcp-core.exe");
        assert_eq!(p[0].working_directory, "R");
        assert!(p[0].arguments.is_empty());
    }

    /// MUTAZIONE: far saltare in silenzio le voci senza binario (come faceva il
    /// generatore precedente, che semplicemente non le conosceva) e questo
    /// rosseggia.
    #[test]
    fn un_fossile_nel_catalogo_e_un_errore_non_un_silenzio() {
        let cat = vec![
            voce("mcp-core", Some("nexus-mcp-core")),
            voce("billing-service", Some("nexus-billing")),
        ];
        let bins = BTreeMap::from([("mcp-core".to_string(), "mcp-core".to_string())]);
        let errs = plan(&cat, &bins, &[], &ordine(&["nexus-mcp-core"]), &amb())
            .expect_err("il fossile deve fermare il piano");
        assert!(errs.iter().any(|e| matches!(
            e,
            PlanError::BinarioAssente { nome, .. } if nome == "billing-service"
        )));
    }

    /// Il piano non produce MAI un insieme parziale: o e' completo o e' errore.
    #[test]
    fn una_voce_non_risolvibile_non_lascia_passare_le_altre() {
        let cat = vec![
            voce("mcp-core", Some("nexus-mcp-core")),
            voce("fantasma", Some("nexus-fantasma")),
        ];
        let bins = BTreeMap::from([("mcp-core".to_string(), "mcp-core".to_string())]);
        let r = plan(&cat, &bins, &[], &ordine(&["nexus-mcp-core"]), &amb());
        assert!(r.is_err(), "un piano parziale non deve essere un esito possibile");
    }

    /// La condizione che il difetto originale rendeva invisibile.
    #[test]
    fn un_servizio_fuori_dall_ordine_di_avvio_e_un_errore() {
        let cat = vec![
            voce("mcp-core", Some("nexus-mcp-core")),
            voce("browser-bridge-mcp", Some("nexus-browser-bridge")),
        ];
        let bins = BTreeMap::from([
            ("mcp-core".to_string(), "mcp-core".to_string()),
            ("browser-bridge-mcp".to_string(), "browser-bridge-mcp".to_string()),
        ]);
        // Ordine che dimentica browser-bridge: e' com'era dev-start.ps1.
        let errs = plan(&cat, &bins, &[], &ordine(&["nexus-mcp-core"]), &amb())
            .expect_err("l'ordine incompleto deve fermare il piano");
        assert!(errs.iter().any(|e| matches!(
            e,
            PlanError::OrdineIncompleto { mancanti } if mancanti.iter().any(|m| m == "nexus-browser-bridge")
        )));
    }

    #[test]
    fn l_ordine_non_puo_nominare_id_inesistenti() {
        let cat = vec![voce("mcp-core", Some("nexus-mcp-core"))];
        let bins = BTreeMap::from([("mcp-core".to_string(), "mcp-core".to_string())]);
        let errs = plan(
            &cat,
            &bins,
            &[],
            &ordine(&["nexus-mcp-core", "nexus-chat"]),
            &amb(),
        )
        .expect_err("id estraneo");
        assert!(errs
            .iter()
            .any(|e| matches!(e, PlanError::OrdineOrfano { estranei } if estranei.iter().any(|x| x == "nexus-chat"))));
    }

    /// MUTAZIONE: sostituire il confronto misurato con `if nome == "postgres"`
    /// e questo rosseggia al primo rinomino.
    #[test]
    fn il_servizio_che_ospita_il_catalogo_non_si_autogenera() {
        let mut a = amb();
        a.porte.insert("archivio".to_string(), 5433);
        a.porta_db = Some(5433);
        let cat = vec![voce("archivio", Some("nexus-archivio"))];
        let bins = BTreeMap::from([("archivio".to_string(), "archivio".to_string())]);
        let errs = plan(&cat, &bins, &[], &ordine(&[]), &a).expect_err("circolarita'");
        assert!(errs
            .iter()
            .any(|e| matches!(e, PlanError::ServizioDelDb { porta, .. } if *porta == 5433)));
    }

    #[test]
    fn due_voci_con_lo_stesso_id_si_sovrascriverebbero() {
        let cat = vec![voce("a", Some("nexus-x")), voce("b", Some("nexus-x"))];
        let bins = BTreeMap::from([
            ("a".to_string(), "a".to_string()),
            ("b".to_string(), "b".to_string()),
        ]);
        let errs = plan(&cat, &bins, &[], &ordine(&["nexus-x"]), &amb()).expect_err("collisione");
        assert!(errs
            .iter()
            .any(|e| matches!(e, PlanError::IdCollisione { winsw_id, .. } if winsw_id == "nexus-x")));
    }

    /// Il confine fra le due fonti: uno scostamento non puo' coprire un package.
    #[test]
    fn uno_scostamento_non_puo_coprire_un_binario_del_workspace() {
        let cat = vec![voce("mcp-core", Some("nexus-mcp-core"))];
        let bins = BTreeMap::from([("mcp-core".to_string(), "mcp-core".to_string())]);
        let s = ExecOverride {
            catalogo: "mcp-core".into(),
            executable: "altro.exe".into(),
            arguments: Vec::new(),
            working_directory: None,
            env_da_dotenv: None,
            env_letterali: BTreeMap::new(),
            env_da_porta: BTreeMap::new(),
            file: "t.toml".into(),
        };
        let errs = plan(&cat, &bins, &[s], &ordine(&["nexus-mcp-core"]), &amb())
            .expect_err("scostamento su package");
        assert!(errs
            .iter()
            .any(|e| matches!(e, PlanError::ScostamentoSuPackage { nome } if nome == "mcp-core")));
    }
}
