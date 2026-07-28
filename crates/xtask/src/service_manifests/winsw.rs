//! Emissione e rilettura del manifest WinSW.
//!
//! `parse_winsw` estrae i quattro campi che `Start-FromManifest`
//! (deploy/dev-service.ps1) legge davvero: executable, workingdirectory,
//! arguments, env. Serve al confronto con l'esistente (`--check`), NON a
//! rappresentare il consumatore: e' una seconda implementazione del lettore, e
//! un round-trip con essa dimostra solo che sappiamo rileggere il nostro XML.
//! Che il consumatore lo sappia leggere lo misurano i test marcati `windows`,
//! che invocano `deploy/lib/nexus-manifest.ps1` — il lettore vero — sull'XML
//! prodotto qui. La distinzione e' costata sette servizi fermi il 2026-07-28.

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

    /// Il BOM non deve accecare il lettore, altrimenti ogni confronto con
    /// l'esistente direbbe "divergente" a vuoto.
    ///
    /// PREMESSA CORRETTA il 28/07/2026: qui c'era scritto "i manifest gia' sul
    /// disco iniziano con il BOM". Era vero dei manifest del vecchio generatore
    /// PowerShell (`Set-Content -Encoding utf8` in PS 5.1 lo antepone); quelli
    /// che scriviamo noi no — misurati sul disco, iniziano con `3C 21 2D`, cioe'
    /// il marcatore. Il test resta valido per i residui e per i file scritti a
    /// mano, ma non descrive piu' cio' che il generatore produce: una premessa
    /// non verificata invecchia in silenzio e rende il verde un'opinione.
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

    // ── Il consumatore VERO ───────────────────────────────────────────────────
    //
    // I test qui sopra girano tutti contro `parse_winsw`, che NON e' il
    // consumatore: e' una seconda implementazione del lettore, scritta dalla
    // stessa mano nello stesso momento del produttore. Un round-trip fra le due
    // verifica che sappiamo rileggere il nostro XML, non che chi lo consuma lo
    // sappia leggere — e quando le due copie divergono resta verde (regola O).
    //
    // La divergenza e' arrivata il 2026-07-28. `parse_winsw` tollera
    // `<arguments>` assente (`unwrap_or_default`); il lettore PowerShell faceva
    // `$s.arguments`, che sotto lo StrictMode propagato da deploy-local.ps1
    // solleva un'eccezione. Sette servizi su otto non sono partiti dopo un deploy
    // riuscito, con questa suite verde e col test qui sopra che certificava
    // proprio l'omissione del tag.
    //
    // Da qui in giu' si misura il lettore reale, nelle condizioni reali.

    /// Fa leggere il manifest a `deploy/lib/nexus-manifest.ps1` — il lettore che
    /// dev-start.ps1 e dev-service.ps1 usano davvero — con StrictMode attivo,
    /// cioe' nello stato in cui lo mette il deploy.
    #[cfg(windows)]
    fn letto_da_powershell(xml: &str, caso: &str) -> (String, String, String, Vec<String>) {
        const LETTORE: &str = r#"Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'
. '@LIB@'
$m = Read-NexusServiceManifest -Path '@MANIFEST@'
Write-Output ('EXE=' + $m.Executable)
Write-Output ('CWD=' + $m.WorkingDirectory)
Write-Output ('ARGS=' + $m.Arguments)
foreach ($e in @($m.Env)) { Write-Output ('ENV=' + $e.Name + '=' + $e.Value) }
"#;
        let lib = super::super::repo_root()
            .expect("radice del repository")
            .join("deploy")
            .join("lib")
            .join("nexus-manifest.ps1");
        assert!(
            lib.exists(),
            "il lettore dei manifest non esiste: {}",
            lib.display()
        );

        let dir = std::env::temp_dir().join(format!("nexus-manifest-{caso}"));
        std::fs::create_dir_all(&dir).expect("directory temporanea");
        let manifest = dir.join("servizio.xml");
        std::fs::write(&manifest, xml).expect("scrittura manifest");
        let script = dir.join("leggi.ps1");
        std::fs::write(
            &script,
            LETTORE
                .replace("@LIB@", &lib.display().to_string())
                .replace("@MANIFEST@", &manifest.display().to_string()),
        )
        .expect("scrittura script");

        let out = std::process::Command::new("powershell.exe")
            .args(["-NoProfile", "-ExecutionPolicy", "Bypass", "-File"])
            .arg(&script)
            .output()
            .expect("esecuzione di powershell.exe");
        assert!(
            out.status.success(),
            "il lettore ha rifiutato il manifest generato:\n{}",
            String::from_utf8_lossy(&out.stderr)
        );

        let testo = String::from_utf8_lossy(&out.stdout).replace('\r', "");
        let campo = |p: &str| {
            testo
                .lines()
                .find_map(|r| r.strip_prefix(p))
                .unwrap_or_default()
                .to_string()
        };
        let env = testo
            .lines()
            .filter_map(|r| r.strip_prefix("ENV="))
            .map(str::to_string)
            .collect();
        (campo("EXE="), campo("CWD="), campo("ARGS="), env)
    }

    /// Il caso esatto dell'incidente: mcp-core non ha ne' `<arguments>` ne'
    /// `<env>`, perche' il generatore li omette quando sono vuoti.
    ///
    /// MUTAZIONE: rimettere `$s.arguments` al posto di `SelectSingleNode` nel
    /// lettore e questo test rosseggia con il messaggio del difetto reale
    /// ("Impossibile trovare la proprieta' 'arguments' in questo oggetto"),
    /// mentre l'intera suite Rust qui sopra resta verde.
    #[cfg(windows)]
    #[test]
    fn il_lettore_reale_regge_i_tag_opzionali_omessi_dal_generatore() {
        let mut s = servizio();
        s.arguments.clear();
        s.env.clear();
        let xml = emit_winsw(&s);
        let (exe, cwd, args, env) = letto_da_powershell(&xml, "senza-opzionali");
        assert_eq!(exe, s.executable);
        assert_eq!(cwd, s.working_directory);
        assert_eq!(args, "", "un servizio senza argomenti rende la riga vuota");
        assert!(env.is_empty(), "nessuna variabile attesa, trovate: {env:?}");
    }

    /// Non basta non esplodere: quando i tag ci sono, i valori devono ARRIVARE.
    /// Senza questo, un lettore che rendesse sempre stringa vuota passerebbe il
    /// test qui sopra e lo stack partirebbe con gli argomenti persi (garnet
    /// senza `--port` si prende la 6379 per default e il difetto resta latente).
    #[cfg(windows)]
    #[test]
    fn il_lettore_reale_rende_i_valori_quando_i_tag_ci_sono() {
        let s = servizio();
        let (_, _, args, env) = letto_da_powershell(&emit_winsw(&s), "con-opzionali");
        assert_eq!(args, "--port 6379");
        assert_eq!(env, vec!["NODE_ENV=production".to_string()]);
    }
}
