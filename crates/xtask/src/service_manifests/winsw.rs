//! Emissione e rilettura del manifest WinSW.
//!
//! `parse_winsw` estrae ESATTAMENTE i quattro campi che `Start-FromManifest`
//! (deploy/dev-service.ps1) legge davvero: executable, workingdirectory,
//! arguments, env. Il round-trip serve a verificare che cio' che scriviamo sia
//! cio' che il consumatore leggera', non che sappiamo rileggere il nostro XML.

use super::plan::ServizioRisolto;

/// Marcatore in testa a ogni manifest generato: chi lo apre deve sapere che una
/// modifica a mano sara' sovrascritta al prossimo giro.
pub const MARCATORE: &str =
    "<!-- generato da `cargo xtask service-manifests`, non modificare a mano -->";

fn esc(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
}

/// Genera il manifest. Stesso schema che WinSW e dev-service.ps1 gia' consumano.
pub fn emit_winsw(s: &ServizioRisolto) -> String {
    let mut xml = String::new();
    xml.push_str(MARCATORE);
    xml.push('\n');
    xml.push_str("<service>\n");
    xml.push_str(&format!("  <id>{}</id>\n", esc(&s.winsw_id)));
    xml.push_str(&format!("  <name>Nexus {}</name>\n", esc(&s.display)));
    let descr = s
        .descrizione
        .clone()
        .unwrap_or_else(|| format!("Nexus {}", s.display));
    xml.push_str(&format!("  <description>{}</description>\n", esc(&descr)));
    xml.push_str(&format!("  <executable>{}</executable>\n", esc(&s.executable)));
    if !s.arguments.is_empty() {
        xml.push_str(&format!(
            "  <arguments>{}</arguments>\n",
            esc(&s.arguments.join(" "))
        ));
    }
    xml.push_str(&format!(
        "  <workingdirectory>{}</workingdirectory>\n",
        esc(&s.working_directory)
    ));
    for (k, v) in s.env.iter() {
        xml.push_str(&format!(
            "  <env name=\"{}\" value=\"{}\"/>\n",
            esc(k),
            esc(v)
        ));
    }
    xml.push_str("  <log mode=\"roll-by-size\"><sizeThreshold>10240</sizeThreshold><keepFiles>3</keepFiles></log>\n");
    xml.push_str("  <onfailure action=\"restart\" delay=\"5 sec\"/>\n");
    xml.push_str("  <startmode>Automatic</startmode>\n");
    xml.push_str("  <resetfailure>1 hour</resetfailure>\n");
    xml.push_str("  <stopparentprocessfirst>false</stopparentprocessfirst>\n");
    xml.push_str("</service>\n");
    xml
}

/// I soli fatti che il consumatore estrae dal manifest.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct ManifestFacts {
    pub executable: String,
    pub working_directory: String,
    pub arguments: Vec<String>,
    pub env: Vec<(String, String)>,
}

fn unesc(s: &str) -> String {
    s.replace("&quot;", "\"")
        .replace("&gt;", ">")
        .replace("&lt;", "<")
        .replace("&amp;", "&")
}

fn tra(testo: &str, tag: &str) -> Option<String> {
    let apri = format!("<{tag}>");
    let chiudi = format!("</{tag}>");
    let i = testo.find(&apri)? + apri.len();
    let j = testo[i..].find(&chiudi)? + i;
    Some(unesc(testo[i..j].trim()))
}

/// Rilegge un manifest. Tollera il BOM: i manifest gia' su disco ce l'hanno.
pub fn parse_winsw(xml: &str) -> ManifestFacts {
    let xml = xml.trim_start_matches('\u{feff}');
    let arguments = tra(xml, "arguments")
        .map(|a| a.split_whitespace().map(str::to_string).collect())
        .unwrap_or_default();
    let mut env = Vec::new();
    let mut resto = xml;
    while let Some(i) = resto.find("<env ") {
        let dopo = &resto[i..];
        let Some(fine) = dopo.find("/>") else { break };
        let riga = &dopo[..fine];
        let nome = riga
            .split("name=\"")
            .nth(1)
            .and_then(|r| r.split('"').next())
            .map(unesc);
        let valore = riga
            .split("value=\"")
            .nth(1)
            .and_then(|r| r.split('"').next())
            .map(unesc);
        if let (Some(n), Some(v)) = (nome, valore) {
            env.push((n, v));
        }
        resto = &dopo[fine + 2..];
    }
    ManifestFacts {
        executable: tra(xml, "executable").unwrap_or_default(),
        working_directory: tra(xml, "workingdirectory").unwrap_or_default(),
        arguments,
        env,
    }
}

#[cfg(test)]
mod tests {
    use super::super::plan::{Provenienza, ServizioRisolto};
    use super::*;

    fn servizio() -> ServizioRisolto {
        ServizioRisolto {
            winsw_id: "nexus-garnet".into(),
            nome_catalogo: "redis".into(),
            display: "Redis".into(),
            descrizione: Some("Cache & broker".into()),
            executable: "RT/garnet/GarnetServer.exe".into(),
            arguments: vec!["--port".into(), "6379".into()],
            working_directory: "RT/garnet".into(),
            env: vec![("NODE_ENV".into(), "production".into())],
            porta: Some(6379),
            provenienza: Provenienza::Scostamento {
                file: "t.toml".into(),
                indice: 0,
            },
        }
    }

    /// I quattro campi che il consumatore legge sopravvivono al round-trip.
    /// MUTAZIONE: rinominare `<workingdirectory>` in `<workdir>` e questo
    /// rosseggia — mentre un test che si limitasse a confrontare la stringa
    /// appena composta con se stessa resterebbe verde.
    #[test]
    fn i_campi_consumati_sopravvivono_al_round_trip() {
        let s = servizio();
        let f = parse_winsw(&emit_winsw(&s));
        assert_eq!(f.executable, s.executable);
        assert_eq!(f.working_directory, s.working_directory);
        assert_eq!(f.arguments, s.arguments);
        assert_eq!(f.env, s.env);
    }

    #[test]
    fn il_marcatore_e_in_testa() {
        assert!(emit_winsw(&servizio()).starts_with(MARCATORE));
    }

    /// I manifest gia' sul disco iniziano con il BOM: il lettore deve vederli,
    /// altrimenti ogni confronto con l'esistente direbbe "divergente" a vuoto.
    #[test]
    fn il_bom_dei_manifest_esistenti_non_acceca_il_lettore() {
        let con_bom = format!("\u{feff}{}", emit_winsw(&servizio()));
        assert_eq!(parse_winsw(&con_bom).executable, "RT/garnet/GarnetServer.exe");
    }

    /// L'escape sopravvive: un path con `&` non deve rompere l'XML.
    #[test]
    fn i_caratteri_speciali_fanno_round_trip() {
        let mut s = servizio();
        s.working_directory = "C:/Program Files & Co".into();
        let f = parse_winsw(&emit_winsw(&s));
        assert_eq!(f.working_directory, "C:/Program Files & Co");
    }

    #[test]
    fn un_servizio_senza_argomenti_non_emette_il_tag() {
        let mut s = servizio();
        s.arguments.clear();
        let xml = emit_winsw(&s);
        assert!(!xml.contains("<arguments>"));
        assert!(parse_winsw(&xml).arguments.is_empty());
    }
}
