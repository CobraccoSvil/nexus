//! Scostamenti di esecuzione per i servizi che il workspace non costruisce, e
//! ordine di avvio. Entrambi da `deploy/service-exec-overrides.toml`, versionato.

use std::collections::BTreeMap;

use anyhow::Context;
use nexus_service_catalog::CatalogEntry;
use serde::Deserialize;

use super::plan::{Ambiente, PlanError};

/// Una voce del TOML. I campi env dichiarano NOMI di chiavi, mai valori.
#[derive(Debug, Clone, Deserialize)]
pub struct ExecOverride {
    pub catalogo: String,
    pub executable: String,
    #[serde(default)]
    pub arguments: Vec<String>,
    #[serde(default)]
    pub working_directory: Option<String>,
    /// `"*"` proietta tutte le chiavi del .env; altrimenti nessuna.
    #[serde(default)]
    pub env_da_dotenv: Option<String>,
    #[serde(default)]
    pub env_letterali: BTreeMap<String, String>,
    /// Nome variabile -> chiave settings della porta.
    #[serde(default)]
    pub env_da_porta: BTreeMap<String, String>,
    /// File di provenienza, per l'indice generato. Non arriva dal TOML.
    #[serde(skip)]
    pub file: String,
}

/// Ordine di avvio e attese.
#[derive(Debug, Clone, Default, Deserialize)]
pub struct Ordine {
    #[serde(default)]
    pub avvio: Vec<String>,
    #[serde(default)]
    pub attesa_dopo: BTreeMap<String, u64>,
}

#[derive(Debug, Deserialize)]
struct FileScostamenti {
    #[serde(default, rename = "servizio")]
    servizi: Vec<ExecOverride>,
    #[serde(default)]
    ordine: Ordine,
}

/// Legge il file versionato. L'assenza e' un errore, non una lista vuota: un
/// file mancante produrrebbe un piano senza i tre servizi non-workspace, cioe'
/// esattamente la sparizione silenziosa di manifest che si vuole impedire.
pub fn carica(path: &std::path::Path) -> anyhow::Result<(Vec<ExecOverride>, Ordine)> {
    let raw = std::fs::read_to_string(path)
        .with_context(|| format!("lettura di {}", path.display()))?;
    let f: FileScostamenti =
        toml::from_str(&raw).with_context(|| format!("{} non e' un TOML valido", path.display()))?;
    let nome_file = path.display().to_string();
    let servizi = f
        .servizi
        .into_iter()
        .map(|mut s| {
            s.file = nome_file.clone();
            s
        })
        .collect();
    Ok((servizi, f.ordine))
}

/// Espande i placeholder di un testo. Qualunque `${...}` non riconosciuto e'
/// un errore: una sostituzione mancata in silenzio produrrebbe un path
/// letterale `${RUNTIME}/qdrant` e un servizio che non parte.
fn espandi(
    testo: &str,
    nome: &str,
    porta: Option<u16>,
    amb: &Ambiente,
) -> Result<String, PlanError> {
    let mut out = String::with_capacity(testo.len());
    let mut resto = testo;
    while let Some(i) = resto.find("${") {
        out.push_str(&resto[..i]);
        let dopo = &resto[i + 2..];
        let Some(j) = dopo.find('}') else {
            return Err(PlanError::PlaceholderIgnoto {
                nome: nome.to_string(),
                testo: testo.to_string(),
            });
        };
        let chiave = &dopo[..j];
        let valore = match chiave {
            "REPO" => amb.repo_root.clone(),
            "RUNTIME" => amb.runtime_root.clone(),
            "EXE" => amb.exe_ext.clone(),
            "NODE" => amb.node.clone().ok_or_else(|| PlanError::PlaceholderIgnoto {
                nome: nome.to_string(),
                testo: "${NODE}: eseguibile node non trovato nel PATH".to_string(),
            })?,
            "PORT" => porta
                .ok_or_else(|| PlanError::PortaNonRisolvibile {
                    nome: nome.to_string(),
                    chiave: "porta del servizio".to_string(),
                })?
                .to_string(),
            _ => {
                return Err(PlanError::PlaceholderIgnoto {
                    nome: nome.to_string(),
                    testo: testo.to_string(),
                })
            }
        };
        out.push_str(&valore);
        resto = &dopo[j + 1..];
    }
    out.push_str(resto);
    Ok(out)
}

type SpecRisolta = (String, Vec<String>, String, Vec<(String, String)>);

/// Risolve uno scostamento in (executable, arguments, working_directory, env).
pub fn risolvi(
    s: &ExecOverride,
    e: &CatalogEntry,
    porta: Option<u16>,
    amb: &Ambiente,
) -> Result<SpecRisolta, Vec<PlanError>> {
    let mut errori = Vec::new();
    let mut esp = |t: &str| match espandi(t, &e.name, porta, amb) {
        Ok(v) => v,
        Err(err) => {
            errori.push(err);
            String::new()
        }
    };

    let executable = esp(&s.executable);
    let arguments: Vec<String> = s.arguments.iter().map(|a| esp(a)).collect();
    let working_directory = match s.working_directory.as_deref() {
        Some(w) => esp(w),
        None => amb.repo_root.clone(),
    };

    let mut env: Vec<(String, String)> = Vec::new();
    if s.env_da_dotenv.as_deref() == Some("*") {
        for (k, v) in amb.dotenv.iter() {
            env.push((k.clone(), v.clone()));
        }
    }
    for (k, v) in s.env_letterali.iter() {
        env.push((k.clone(), v.clone()));
    }
    for (var, chiave) in s.env_da_porta.iter() {
        match amb.porte.get(&e.name) {
            Some(p) => env.push((var.clone(), p.to_string())),
            None => errori.push(PlanError::PortaNonRisolvibile {
                nome: e.name.clone(),
                chiave: chiave.clone(),
            }),
        }
    }
    // Le ultime vincono: letterali e porta sovrascrivono il .env proiettato.
    env.dedup_by(|a, b| a.0 == b.0);

    if errori.is_empty() {
        Ok((executable, arguments, working_directory, env))
    } else {
        Err(errori)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn amb() -> Ambiente {
        Ambiente {
            repo_root: "R".into(),
            runtime_root: "RT".into(),
            exe_ext: ".exe".into(),
            node: Some("N/node.exe".into()),
            ..Default::default()
        }
    }

    #[test]
    fn i_placeholder_noti_si_espandono() {
        let a = amb();
        assert_eq!(
            espandi("${RUNTIME}/qdrant/qdrant${EXE}", "qdrant", None, &a).expect("ok"),
            "RT/qdrant/qdrant.exe"
        );
        assert_eq!(espandi("${NODE}", "web-ide", None, &a).expect("ok"), "N/node.exe");
    }

    /// MUTAZIONE: lasciare passare i placeholder ignoti come testo letterale e
    /// questo rosseggia — un `${RUNTIEM}` scritto male diventerebbe un path che
    /// non esiste, e il servizio non partirebbe senza dire perche'.
    #[test]
    fn un_placeholder_ignoto_non_passa_in_silenzio() {
        let a = amb();
        let e = espandi("${RUNTIEM}/x", "qdrant", None, &a).expect_err("deve fallire");
        assert!(matches!(e, PlanError::PlaceholderIgnoto { .. }));
    }

    /// La porta degli argomenti e' quella risolta, non una costante.
    #[test]
    fn la_porta_degli_argomenti_viene_dalla_risoluzione() {
        let a = amb();
        assert_eq!(
            espandi("${PORT}", "redis", Some(6379), &a).expect("ok"),
            "6379"
        );
        assert_eq!(
            espandi("${PORT}", "redis", Some(6380), &a).expect("ok"),
            "6380"
        );
        assert!(espandi("${PORT}", "redis", None, &a).is_err());
    }

    #[test]
    fn il_file_versionato_del_repo_si_carica() {
        // Legge il TOML VERO del repo, non uno inventato: se una voce viene
        // aggiunta o rinominata, questo test la vede (regola O).
        let root = super::super::repo_root().expect("repo root");
        let p = root.join("deploy/service-exec-overrides.toml");
        let (servizi, ordine) = carica(&p).expect("il file versionato deve essere valido");
        assert!(
            servizi.iter().any(|s| s.catalogo == "web-ide"),
            "web-ide deve avere uno scostamento: non e' un binario del workspace"
        );
        assert!(
            !ordine.avvio.is_empty(),
            "l'ordine di avvio non puo' essere vuoto"
        );
    }
}
