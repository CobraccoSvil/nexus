//! Renderer .docx in Rust — PUNTO UNICO (regola L) della generazione documenti
//! Word a partire dal JSON strutturato `{"sections":[...]}`.
//!
//! Sostituisce il vecchio percorso gRPC `generate_document` verso il brain Python
//! (`brain/documents/generator.py` + `styles.py`), ultimo residuo AI-adiacente che
//! teneva mcp-core legato al brain per il rendering documenti. Verso zero-Python:
//! qui il .docx viene assemblato interamente in Rust senza alcun round-trip di
//! rete.
//!
//! ## Formato OOXML
//! Un `.docx` e' un archivio ZIP (OPC, Open Packaging Conventions) con un set
//! minimo di parti XML. Generiamo a mano:
//!   - `[Content_Types].xml`     — mappa estensioni/parti -> content type
//!   - `_rels/.rels`             — relazione root -> documento principale
//!   - `word/_rels/document.xml.rels` — relazioni del documento (header/footer)
//!   - `word/document.xml`       — corpo (paragrafi, heading, tabelle, code)
//!   - `word/styles.xml`         — stili Normal / Heading1-3 / list / code / table
//!   - `word/header1.xml`        — intestazione (progetto | titolo | versione)
//!   - `word/footer1.xml`        — pie' di pagina con campo PAGE
//!
//! La struttura e' fissa (nessun namespace dinamico), quindi l'XML e' costruito
//! per concatenazione con escaping esplicito dei valori variabili. Word/LibreOffice
//! aprono il file senza riparazione.
//!
//! ## Parita' con il generatore Python
//! Stili (font Calibri 11, colore testo 1A1A1A, heading blu 1B3A5C alle taglie
//! 18/14/12pt, margini 2.5cm), cover page, header/footer, riconoscimento bullet
//! (`- `/`* `), liste numerate (`1.`/`1)`), tabelle e blocchi di codice replicano
//! 1:1 `brain/documents/styles.py`. Il `page_count` usa la stessa euristica
//! (~3000 caratteri/pagina) di `generator.py`.

use serde_json::Value;
use std::io::Write as _;
use std::path::Path;
use zip::write::SimpleFileOptions;

/// Esito del rendering, paritetico alla tupla storica di `generate_document`.
pub struct RenderedDoc {
    pub file_path: String,
    pub page_count: i32,
    pub section_count: i32,
}

#[derive(Debug, thiserror::Error)]
pub enum DocxError {
    #[error("JSON content non valido: {0}")]
    InvalidJson(String),
    #[error("errore I/O scrittura .docx: {0}")]
    Io(#[from] std::io::Error),
    #[error("errore ZIP .docx: {0}")]
    Zip(#[from] zip::result::ZipError),
}

// ───────────────────────────────────────────────────────────────────────────
// API pubblica
// ───────────────────────────────────────────────────────────────────────────

/// Renderizza un documento `.docx` su `output_path` a partire dal JSON
/// strutturato. Replica `DocumentGenerator.generate` (Python): parse del
/// content, cover + header/footer + sezioni ricorsive, scrittura atomica
/// (file temporaneo + rename), euristica page_count.
///
/// `content_json` puo' essere sia una stringa JSON sia gia' un oggetto: nel path
/// vivo di `handle_doc_generate` arriva come stringa (serializzata da `content`).
pub fn render_document(
    doc_type: &str,
    content_json: &str,
    output_path: &str,
    standard: &str,
    title: &str,
    project_name: &str,
) -> Result<RenderedDoc, DocxError> {
    let content: Value =
        serde_json::from_str(content_json).map_err(|e| DocxError::InvalidJson(e.to_string()))?;

    let version = content
        .get("version")
        .and_then(Value::as_str)
        .unwrap_or("1.0.0")
        .to_string();
    let title = if title.is_empty() {
        default_title_for(doc_type)
    } else {
        title.to_string()
    };
    let standard = if standard.is_empty() {
        "minimal"
    } else {
        standard
    };

    let empty: Vec<Value> = Vec::new();
    let sections = content
        .get("sections")
        .and_then(Value::as_array)
        .unwrap_or(&empty);

    // Corpo del documento: cover page, poi le sezioni ricorsive.
    let mut body = String::new();
    body.push_str(&render_cover(&title, project_name, &version, standard));

    let mut section_count = 0i32;
    for sec in sections {
        section_count += render_section(&mut body, sec, 1);
    }

    // Proprieta' di sezione finali (margini + riferimenti header/footer). Devono
    // essere l'ultimo elemento del <w:body> (regola OOXML).
    body.push_str(&section_properties());

    let document_xml = wrap_document(&body);

    // Page count: stessa euristica del generatore Python (~3000 char/pagina,
    // sul solo testo `content` di primo livello).
    let total_chars: usize = sections
        .iter()
        .map(|s| as_text(s.get("content")).chars().count())
        .sum();
    let page_count = (total_chars / 3000 + 1).max(1) as i32;

    // Scrittura atomica: prima su .tmp.docx, poi rename sull'output finale.
    let out = Path::new(output_path);
    if let Some(parent) = out.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let tmp = out.with_extension("tmp.docx");

    write_docx_zip(
        &tmp,
        &document_xml,
        &header_xml(project_name, &title, &version),
        &footer_xml(),
    )?;

    if out.exists() {
        std::fs::remove_file(out)?;
    }
    std::fs::rename(&tmp, out)?;

    Ok(RenderedDoc {
        file_path: output_path.to_string(),
        page_count,
        section_count,
    })
}

// ───────────────────────────────────────────────────────────────────────────
// Coercion content -> testo (replica `_as_text` di generator.py)
// ───────────────────────────────────────────────────────────────────────────

/// Coerce un valore `content` arbitrario a stringa renderizzabile. Il modello
/// docs_generator a volte annida liste/oggetti dentro `content` invece di una
/// stringa: qui normalizziamo sempre a testo leggibile (parita' con `_as_text`).
fn as_text(value: Option<&Value>) -> String {
    match value {
        None | Some(Value::Null) => String::new(),
        Some(Value::String(s)) => s.clone(),
        Some(Value::Array(items)) => items
            .iter()
            .filter(|v| !v.is_null())
            .map(|v| as_text(Some(v)))
            .collect::<Vec<_>>()
            .join("\n"),
        Some(Value::Object(map)) => {
            for key in ["content", "text", "value"] {
                if let Some(v) = map.get(key) {
                    return as_text(Some(v));
                }
            }
            serde_json::to_string_pretty(value.unwrap()).unwrap_or_default()
        }
        Some(other) => other.to_string(),
    }
}

// ───────────────────────────────────────────────────────────────────────────
// Rendering del corpo
// ───────────────────────────────────────────────────────────────────────────

/// Renderizza una sezione e le sue sottosezioni ricorsivamente. Ritorna il
/// numero di nodi sezione renderizzati (parita' con `_render_section`).
fn render_section(body: &mut String, section: &Value, level: u8) -> i32 {
    let number = section.get("number").and_then(Value::as_str).unwrap_or("");
    let title = section.get("title").and_then(Value::as_str).unwrap_or("");
    let content = as_text(section.get("content"));

    let heading_level = level.min(3);
    let heading_text = if number.is_empty() {
        title.to_string()
    } else {
        format!("{number}. {title}")
    };
    body.push_str(&heading_paragraph(&heading_text, heading_level));

    let mut count = 1i32;

    if !content.is_empty() {
        render_content(body, &content);
    }

    // Tabella opzionale.
    if let Some(table) = section.get("table").and_then(Value::as_object) {
        let headers: Vec<String> = table
            .get("headers")
            .and_then(Value::as_array)
            .map(|a| a.iter().map(|v| as_text(Some(v))).collect())
            .unwrap_or_default();
        let rows: Vec<Vec<String>> = table
            .get("rows")
            .and_then(Value::as_array)
            .map(|rows| {
                rows.iter()
                    .map(|r| {
                        r.as_array()
                            .map(|cells| cells.iter().map(|c| as_text(Some(c))).collect())
                            .unwrap_or_default()
                    })
                    .collect()
            })
            .unwrap_or_default();
        if !headers.is_empty() && !rows.is_empty() {
            body.push_str(&render_table(&headers, &rows));
        }
    }

    // Blocco di codice opzionale.
    if let Some(code) = section.get("code") {
        match code {
            Value::Object(map) => {
                let code_text = map
                    .get("content")
                    .map(|v| as_text(Some(v)))
                    .unwrap_or_default();
                let lang = map.get("language").and_then(Value::as_str).unwrap_or("");
                body.push_str(&render_code_block(&code_text, lang));
            }
            Value::String(s) => body.push_str(&render_code_block(s, "")),
            _ => {}
        }
    }

    // Sottosezioni.
    if let Some(subs) = section.get("subsections").and_then(Value::as_array) {
        for sub in subs {
            count += render_section(body, sub, level + 1);
        }
    }

    count
}

/// Renderizza il testo `content` riga per riga: bullet (`- `/`* `), liste
/// numerate (`1.`/`1)`), salto dei fence ```` ``` ````, paragrafo semplice
/// altrimenti (parita' con `_render_content`).
fn render_content(body: &mut String, content: &str) {
    for line in content.trim().split('\n') {
        let stripped = line.trim();
        if stripped.is_empty() {
            continue;
        }
        if let Some(rest) = stripped
            .strip_prefix("- ")
            .or_else(|| stripped.strip_prefix("* "))
        {
            body.push_str(&list_paragraph(rest, "ListBullet"));
        } else if is_numbered_list_line(stripped) {
            // Salta il primo carattere (cifra) + il delimitatore.
            let rest = stripped[2..].trim_start();
            body.push_str(&list_paragraph(rest, "ListNumber"));
        } else if stripped.starts_with("```") {
            // Fence mermaid/markdown: ignorata come nel generatore Python.
            continue;
        } else {
            body.push_str(&plain_paragraph(stripped));
        }
    }
}

/// True per righe del tipo `1. testo` o `1) testo` (singola cifra iniziale).
fn is_numbered_list_line(s: &str) -> bool {
    let bytes = s.as_bytes();
    bytes.len() > 2 && bytes[0].is_ascii_digit() && (bytes[1] == b'.' || bytes[1] == b')')
}

// ───────────────────────────────────────────────────────────────────────────
// Frammenti XML del corpo
// ───────────────────────────────────────────────────────────────────────────

fn plain_paragraph(text: &str) -> String {
    format!(
        "<w:p><w:r><w:t xml:space=\"preserve\">{}</w:t></w:r></w:p>",
        xml_escape(text)
    )
}

fn list_paragraph(text: &str, style: &str) -> String {
    format!(
        "<w:p><w:pPr><w:pStyle w:val=\"{style}\"/></w:pPr><w:r><w:t xml:space=\"preserve\">{}</w:t></w:r></w:p>",
        xml_escape(text)
    )
}

fn heading_paragraph(text: &str, level: u8) -> String {
    format!(
        "<w:p><w:pPr><w:pStyle w:val=\"Heading{level}\"/></w:pPr><w:r><w:t xml:space=\"preserve\">{}</w:t></w:r></w:p>",
        xml_escape(text)
    )
}

/// Blocco di codice monospace (Consolas 9pt, grigio scuro). Eventuale etichetta
/// `[lang]` su run separato (parita' con `add_code_block`).
fn render_code_block(code: &str, language: &str) -> String {
    let mut runs = String::new();
    if !language.is_empty() {
        runs.push_str(&format!(
            "<w:r><w:rPr><w:sz w:val=\"16\"/><w:color w:val=\"999999\"/></w:rPr><w:t xml:space=\"preserve\">[{}]</w:t></w:r>",
            xml_escape(language)
        ));
    }
    // Le righe del codice diventano <w:br/> per preservare l'andata a capo.
    let mut first = true;
    let mut code_runs = String::new();
    for line in code.split('\n') {
        if !first {
            code_runs.push_str("<w:br/>");
        }
        code_runs.push_str(&format!(
            "<w:t xml:space=\"preserve\">{}</w:t>",
            xml_escape(line)
        ));
        first = false;
    }
    runs.push_str(&format!(
        "<w:r><w:rPr><w:rFonts w:ascii=\"Consolas\" w:hAnsi=\"Consolas\"/><w:sz w:val=\"18\"/><w:color w:val=\"2D2D2D\"/></w:rPr>{code_runs}</w:r>"
    ));
    format!("<w:p>{runs}</w:p>")
}

/// Tabella con riga di intestazione in grassetto (parita' con `add_table`).
fn render_table(headers: &[String], rows: &[Vec<String>]) -> String {
    let ncols = headers.len();
    let mut out = String::from(
        "<w:tbl><w:tblPr><w:tblStyle w:val=\"LightGrid\"/><w:tblW w:w=\"0\" w:type=\"auto\"/>\
         <w:tblBorders>\
         <w:top w:val=\"single\" w:sz=\"4\" w:color=\"BFBFBF\"/>\
         <w:left w:val=\"single\" w:sz=\"4\" w:color=\"BFBFBF\"/>\
         <w:bottom w:val=\"single\" w:sz=\"4\" w:color=\"BFBFBF\"/>\
         <w:right w:val=\"single\" w:sz=\"4\" w:color=\"BFBFBF\"/>\
         <w:insideH w:val=\"single\" w:sz=\"4\" w:color=\"BFBFBF\"/>\
         <w:insideV w:val=\"single\" w:sz=\"4\" w:color=\"BFBFBF\"/>\
         </w:tblBorders></w:tblPr>",
    );
    // Griglia colonne.
    out.push_str("<w:tblGrid>");
    for _ in 0..ncols {
        out.push_str("<w:gridCol/>");
    }
    out.push_str("</w:tblGrid>");

    // Riga header (grassetto, 10pt).
    out.push_str("<w:tr>");
    for h in headers {
        out.push_str(&format!(
            "<w:tc><w:p><w:r><w:rPr><w:b/><w:sz w:val=\"20\"/></w:rPr><w:t xml:space=\"preserve\">{}</w:t></w:r></w:p></w:tc>",
            xml_escape(h)
        ));
    }
    out.push_str("</w:tr>");

    // Righe dati (10pt).
    for row in rows {
        out.push_str("<w:tr>");
        for col in 0..ncols {
            let cell = row.get(col).map(String::as_str).unwrap_or("");
            out.push_str(&format!(
                "<w:tc><w:p><w:r><w:rPr><w:sz w:val=\"20\"/></w:rPr><w:t xml:space=\"preserve\">{}</w:t></w:r></w:p></w:tc>",
                xml_escape(cell)
            ));
        }
        out.push_str("</w:tr>");
    }
    out.push_str("</w:tbl>");
    // Paragrafo vuoto dopo la tabella (richiesto da OOXML se la tabella e'
    // seguita da altri elementi nello stesso livello).
    out.push_str("<w:p/>");
    out
}

/// Cover page: spazi, nome progetto (grigio 14pt), titolo (blu 28pt grassetto),
/// versione, eventuale standard, page break (parita' con `add_cover_page`).
fn render_cover(title: &str, project_name: &str, version: &str, standard: &str) -> String {
    let mut out = String::new();
    // 4 paragrafi vuoti come spaziatura iniziale.
    for _ in 0..4 {
        out.push_str("<w:p/>");
    }
    // Nome progetto in maiuscolo.
    out.push_str(&format!(
        "<w:p><w:pPr><w:jc w:val=\"center\"/></w:pPr><w:r><w:rPr><w:rFonts w:ascii=\"Calibri\" w:hAnsi=\"Calibri\"/><w:sz w:val=\"28\"/><w:color w:val=\"808080\"/></w:rPr><w:t xml:space=\"preserve\">{}</w:t></w:r></w:p>",
        xml_escape(&project_name.to_uppercase())
    ));
    // Titolo.
    out.push_str(&format!(
        "<w:p><w:pPr><w:jc w:val=\"center\"/></w:pPr><w:r><w:rPr><w:rFonts w:ascii=\"Calibri\" w:hAnsi=\"Calibri\"/><w:b/><w:sz w:val=\"56\"/><w:color w:val=\"1B3A5C\"/></w:rPr><w:t xml:space=\"preserve\">{}</w:t></w:r></w:p>",
        xml_escape(title)
    ));
    // Versione.
    out.push_str(&format!(
        "<w:p><w:pPr><w:jc w:val=\"center\"/></w:pPr><w:r><w:rPr><w:sz w:val=\"28\"/><w:color w:val=\"606060\"/></w:rPr><w:t xml:space=\"preserve\">Versione {}</w:t></w:r></w:p>",
        xml_escape(version)
    ));
    // Standard (se non minimal).
    if !standard.is_empty() && standard != "minimal" {
        let label = match standard {
            "ieee830" => "IEEE 830 / ISO 29148",
            "iso29148" => "ISO/IEC/IEEE 29148:2018",
            other => other,
        };
        out.push_str(&format!(
            "<w:p><w:pPr><w:jc w:val=\"center\"/></w:pPr><w:r><w:rPr><w:sz w:val=\"20\"/><w:color w:val=\"999999\"/></w:rPr><w:t xml:space=\"preserve\">Standard: {}</w:t></w:r></w:p>",
            xml_escape(label)
        ));
    }
    // Page break dopo la cover.
    out.push_str("<w:p><w:r><w:br w:type=\"page\"/></w:r></w:p>");
    out
}

/// Titolo di default per doc_type (parita' con `templates.get_template` →
/// `title_default`). Usato solo se il chiamante non passa un titolo esplicito;
/// nel path vivo il titolo arriva sempre valorizzato.
fn default_title_for(doc_type: &str) -> String {
    match doc_type {
        "functional_analysis" => "Analisi Funzionale",
        "technical_analysis" => "Analisi Tecnica",
        "er_diagram" => "Diagramma Entity-Relationship",
        "project_management" => "Documento di Gestione Progetto",
        "release_notes" => "Note di Rilascio",
        _ => "Documento",
    }
    .to_string()
}

// ───────────────────────────────────────────────────────────────────────────
// Proprieta' di sezione, header/footer
// ───────────────────────────────────────────────────────────────────────────

/// `<w:sectPr>` finale: margini 2.5cm (1417 twip) e riferimenti a header/footer.
fn section_properties() -> String {
    // 2.5 cm = 1417 twip (1 cm = 566.93 twip). Word usa twip (1/20 pt).
    String::from(
        "<w:sectPr>\
         <w:headerReference w:type=\"default\" r:id=\"rIdHeader\"/>\
         <w:footerReference w:type=\"default\" r:id=\"rIdFooter\"/>\
         <w:pgSz w:w=\"11906\" w:h=\"16838\"/>\
         <w:pgMar w:top=\"1417\" w:right=\"1417\" w:bottom=\"1417\" w:left=\"1417\" w:header=\"708\" w:footer=\"708\" w:gutter=\"0\"/>\
         </w:sectPr>",
    )
}

/// Header: "progetto | titolo | vN" allineato a destra, 8pt grigio.
fn header_xml(project_name: &str, title: &str, version: &str) -> String {
    let text = format!("{project_name}  |  {title}  |  v{version}");
    format!(
        "{}<w:hdr xmlns:w=\"http://schemas.openxmlformats.org/wordprocessingml/2006/main\">\
         <w:p><w:pPr><w:jc w:val=\"right\"/></w:pPr><w:r><w:rPr><w:sz w:val=\"16\"/><w:color w:val=\"808080\"/></w:rPr><w:t xml:space=\"preserve\">{}</w:t></w:r></w:p>\
         </w:hdr>",
        XML_DECL,
        xml_escape(&text)
    )
}

/// Footer: campo PAGE centrato, 8pt grigio.
fn footer_xml() -> String {
    format!(
        "{}<w:ftr xmlns:w=\"http://schemas.openxmlformats.org/wordprocessingml/2006/main\">\
         <w:p><w:pPr><w:jc w:val=\"center\"/></w:pPr>\
         <w:r><w:rPr><w:sz w:val=\"16\"/><w:color w:val=\"808080\"/></w:rPr><w:fldChar w:fldCharType=\"begin\"/></w:r>\
         <w:r><w:rPr><w:sz w:val=\"16\"/><w:color w:val=\"808080\"/></w:rPr><w:instrText xml:space=\"preserve\"> PAGE </w:instrText></w:r>\
         <w:r><w:rPr><w:sz w:val=\"16\"/><w:color w:val=\"808080\"/></w:rPr><w:fldChar w:fldCharType=\"end\"/></w:r>\
         </w:p></w:ftr>",
        XML_DECL
    )
}

// ───────────────────────────────────────────────────────────────────────────
// Parti statiche del pacchetto OOXML
// ───────────────────────────────────────────────────────────────────────────

const XML_DECL: &str = "<?xml version=\"1.0\" encoding=\"UTF-8\" standalone=\"yes\"?>\r\n";

fn wrap_document(body: &str) -> String {
    format!(
        "{XML_DECL}<w:document xmlns:w=\"http://schemas.openxmlformats.org/wordprocessingml/2006/main\" \
         xmlns:r=\"http://schemas.openxmlformats.org/officeDocument/2006/relationships\">\
         <w:body>{body}</w:body></w:document>"
    )
}

fn content_types_xml() -> String {
    format!(
        "{XML_DECL}<Types xmlns=\"http://schemas.openxmlformats.org/package/2006/content-types\">\
         <Default Extension=\"rels\" ContentType=\"application/vnd.openxmlformats-package.relationships+xml\"/>\
         <Default Extension=\"xml\" ContentType=\"application/xml\"/>\
         <Override PartName=\"/word/document.xml\" ContentType=\"application/vnd.openxmlformats-officedocument.wordprocessingml.document.main+xml\"/>\
         <Override PartName=\"/word/styles.xml\" ContentType=\"application/vnd.openxmlformats-officedocument.wordprocessingml.styles+xml\"/>\
         <Override PartName=\"/word/header1.xml\" ContentType=\"application/vnd.openxmlformats-officedocument.wordprocessingml.header+xml\"/>\
         <Override PartName=\"/word/footer1.xml\" ContentType=\"application/vnd.openxmlformats-officedocument.wordprocessingml.footer+xml\"/>\
         </Types>"
    )
}

fn root_rels_xml() -> String {
    format!(
        "{XML_DECL}<Relationships xmlns=\"http://schemas.openxmlformats.org/package/2006/relationships\">\
         <Relationship Id=\"rId1\" Type=\"http://schemas.openxmlformats.org/officeDocument/2006/relationships/officeDocument\" Target=\"word/document.xml\"/>\
         </Relationships>"
    )
}

fn document_rels_xml() -> String {
    format!(
        "{XML_DECL}<Relationships xmlns=\"http://schemas.openxmlformats.org/package/2006/relationships\">\
         <Relationship Id=\"rId1\" Type=\"http://schemas.openxmlformats.org/officeDocument/2006/relationships/styles\" Target=\"styles.xml\"/>\
         <Relationship Id=\"rIdHeader\" Type=\"http://schemas.openxmlformats.org/officeDocument/2006/relationships/header\" Target=\"header1.xml\"/>\
         <Relationship Id=\"rIdFooter\" Type=\"http://schemas.openxmlformats.org/officeDocument/2006/relationships/footer\" Target=\"footer1.xml\"/>\
         </Relationships>"
    )
}

/// `word/styles.xml`: Normal (Calibri 11, 1A1A1A), Heading1-3 (blu 1B3A5C,
/// 18/14/12pt grassetto), ListBullet, ListNumber. Le taglie OOXML sono in
/// half-point (Pt(18) -> 36). Replica `apply_base_styles`.
fn styles_xml() -> String {
    format!(
        "{XML_DECL}<w:styles xmlns:w=\"http://schemas.openxmlformats.org/wordprocessingml/2006/main\">\
         <w:docDefaults><w:rPrDefault><w:rPr>\
         <w:rFonts w:ascii=\"Calibri\" w:hAnsi=\"Calibri\"/><w:sz w:val=\"22\"/><w:color w:val=\"1A1A1A\"/>\
         </w:rPr></w:rPrDefault></w:docDefaults>\
         <w:style w:type=\"paragraph\" w:default=\"1\" w:styleId=\"Normal\">\
         <w:name w:val=\"Normal\"/><w:pPr><w:spacing w:after=\"120\" w:line=\"276\" w:lineRule=\"auto\"/></w:pPr>\
         <w:rPr><w:rFonts w:ascii=\"Calibri\" w:hAnsi=\"Calibri\"/><w:sz w:val=\"22\"/><w:color w:val=\"1A1A1A\"/></w:rPr></w:style>\
         {h1}{h2}{h3}\
         <w:style w:type=\"paragraph\" w:styleId=\"ListBullet\">\
         <w:name w:val=\"List Bullet\"/><w:basedOn w:val=\"Normal\"/>\
         <w:pPr><w:numPr><w:ilvl w:val=\"0\"/><w:numId w:val=\"1\"/></w:numPr></w:pPr></w:style>\
         <w:style w:type=\"paragraph\" w:styleId=\"ListNumber\">\
         <w:name w:val=\"List Number\"/><w:basedOn w:val=\"Normal\"/>\
         <w:pPr><w:numPr><w:ilvl w:val=\"0\"/><w:numId w:val=\"2\"/></w:numPr></w:pPr></w:style>\
         </w:styles>",
        XML_DECL = XML_DECL,
        h1 = heading_style(1, 36, 480, 240),
        h2 = heading_style(2, 28, 360, 160),
        h3 = heading_style(3, 24, 240, 120),
    )
}

/// Stile Heading: size in half-point, space_before/after in twip.
fn heading_style(level: u8, sz: u32, before: u32, after: u32) -> String {
    format!(
        "<w:style w:type=\"paragraph\" w:styleId=\"Heading{level}\">\
         <w:name w:val=\"heading {level}\"/><w:basedOn w:val=\"Normal\"/>\
         <w:pPr><w:keepNext/><w:spacing w:before=\"{before}\" w:after=\"{after}\"/>\
         <w:outlineLvl w:val=\"{outline}\"/></w:pPr>\
         <w:rPr><w:rFonts w:ascii=\"Calibri\" w:hAnsi=\"Calibri\"/><w:b/><w:color w:val=\"1B3A5C\"/><w:sz w:val=\"{sz}\"/></w:rPr></w:style>",
        outline = level - 1,
    )
}

// ───────────────────────────────────────────────────────────────────────────
// Assemblaggio ZIP
// ───────────────────────────────────────────────────────────────────────────

fn write_docx_zip(
    path: &Path,
    document_xml: &str,
    header_xml: &str,
    footer_xml: &str,
) -> Result<(), DocxError> {
    let file = std::fs::File::create(path)?;
    let mut zip = zip::ZipWriter::new(file);
    // Deflate: lo stesso filtro usato altrove in mcp-core (feature gia' attiva).
    let opts: SimpleFileOptions =
        SimpleFileOptions::default().compression_method(zip::CompressionMethod::Deflated);

    let parts: [(&str, String); 7] = [
        ("[Content_Types].xml", content_types_xml()),
        ("_rels/.rels", root_rels_xml()),
        ("word/_rels/document.xml.rels", document_rels_xml()),
        ("word/document.xml", document_xml.to_string()),
        ("word/styles.xml", styles_xml()),
        ("word/header1.xml", header_xml.to_string()),
        ("word/footer1.xml", footer_xml.to_string()),
    ];

    for (name, content) in parts {
        zip.start_file(name, opts)?;
        zip.write_all(content.as_bytes())?;
    }
    zip.finish()?;
    Ok(())
}

// ───────────────────────────────────────────────────────────────────────────
// Escaping XML
// ───────────────────────────────────────────────────────────────────────────

/// Escape dei caratteri XML riservati nei valori di testo. Necessario: i content
/// del documento sono testo libero prodotto da un LLM e possono contenere
/// `<`, `>`, `&`, virgolette.
fn xml_escape(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for ch in s.chars() {
        match ch {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&apos;"),
            // Controlla i caratteri di controllo non validi in XML 1.0 (tranne
            // tab/newline/carriage return): li scartiamo per non corrompere il file.
            c if (c as u32) < 0x20 && c != '\t' && c != '\n' && c != '\r' => {}
            c => out.push(c),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Read;

    fn read_zip_part(bytes: &[u8], part: &str) -> String {
        let reader = std::io::Cursor::new(bytes);
        let mut archive = zip::ZipArchive::new(reader).expect("apri docx come zip");
        let mut file = archive.by_name(part).expect("parte presente nel docx");
        let mut s = String::new();
        file.read_to_string(&mut s).expect("leggi parte xml");
        s
    }

    fn render_to_bytes(content_json: &str) -> Vec<u8> {
        let dir = tempfile::tempdir().expect("tempdir");
        let out = dir.path().join("sub").join("doc.docx");
        let res = render_document(
            "functional_analysis",
            content_json,
            out.to_str().unwrap(),
            "ieee830",
            "Analisi Funzionale",
            "Progetto Test",
        )
        .expect("render ok");
        assert!(out.exists(), "il .docx deve esistere su disco");
        assert_eq!(res.file_path, out.to_str().unwrap());
        std::fs::read(&out).expect("leggi docx")
    }

    #[test]
    fn produce_zip_valido_con_parti_obbligatorie() {
        let json =
            r#"{"sections":[{"number":"1","title":"Intro","content":"Testo della sezione."}]}"#;
        let bytes = render_to_bytes(json);
        // Deve essere uno ZIP (magic PK\x03\x04).
        assert_eq!(&bytes[0..2], b"PK");
        // Le parti OOXML obbligatorie devono essere presenti e ben formate.
        for part in [
            "[Content_Types].xml",
            "_rels/.rels",
            "word/document.xml",
            "word/styles.xml",
            "word/header1.xml",
            "word/footer1.xml",
            "word/_rels/document.xml.rels",
        ] {
            let xml = read_zip_part(&bytes, part);
            assert!(xml.contains("<?xml"), "parte {part} deve avere declaration");
        }
    }

    #[test]
    fn document_contiene_heading_e_contenuto() {
        let json = r#"{"sections":[{"number":"1","title":"Introduzione","content":"Prima riga.\n- bullet uno\n1. numerato uno"}]}"#;
        let bytes = render_to_bytes(json);
        let doc = read_zip_part(&bytes, "word/document.xml");
        assert!(doc.contains("Heading1"), "deve usare lo stile Heading1");
        assert!(doc.contains("1. Introduzione"), "heading numerato");
        assert!(doc.contains("Prima riga."), "paragrafo semplice");
        assert!(
            doc.contains("ListBullet"),
            "riga bullet -> stile ListBullet"
        );
        assert!(
            doc.contains("ListNumber"),
            "riga numerata -> stile ListNumber"
        );
    }

    #[test]
    fn escaping_caratteri_xml_riservati() {
        let json =
            r#"{"sections":[{"number":"1","title":"A & B <c>","content":"x < y && z > 0 \"q\""}]}"#;
        let bytes = render_to_bytes(json);
        let doc = read_zip_part(&bytes, "word/document.xml");
        // I caratteri riservati devono essere escapati (niente < grezzo nei valori).
        assert!(doc.contains("A &amp; B &lt;c&gt;"));
        assert!(doc.contains("x &lt; y &amp;&amp; z &gt; 0"));
        // Lo ZIP resta riapribile (validita' strutturale).
        let reader = std::io::Cursor::new(&bytes);
        assert!(zip::ZipArchive::new(reader).is_ok());
    }

    #[test]
    fn tabella_e_code_block() {
        let json = r#"{"sections":[{"number":"2","title":"Dati","content":"intro","table":{"headers":["A","B"],"rows":[["1","2"],["3","4"]]},"code":{"language":"rust","content":"fn main() {}\nlet x = 1;"}}]}"#;
        let bytes = render_to_bytes(json);
        let doc = read_zip_part(&bytes, "word/document.xml");
        assert!(doc.contains("<w:tbl>"), "tabella renderizzata");
        assert!(doc.contains("Consolas"), "code block monospace");
        assert!(doc.contains("[rust]"), "etichetta linguaggio");
        assert!(doc.contains("<w:br/>"), "code block multiriga");
    }

    #[test]
    fn sottosezioni_e_section_count() {
        let json = r#"{"sections":[{"number":"1","title":"S1","content":"a","subsections":[{"number":"1.1","title":"S1.1","content":"b"},{"number":"1.2","title":"S1.2","content":"c"}]},{"number":"2","title":"S2","content":"d"}]}"#;
        let dir = tempfile::tempdir().unwrap();
        let out = dir.path().join("doc.docx");
        let res = render_document(
            "technical_analysis",
            json,
            out.to_str().unwrap(),
            "iso29148",
            "T",
            "P",
        )
        .unwrap();
        // 2 top-level + 2 sottosezioni = 4 nodi sezione (parita' _render_section).
        assert_eq!(res.section_count, 4);
        assert_eq!(res.page_count, 1);
    }

    #[test]
    fn content_non_stringa_viene_coerced() {
        // content come array/oggetto: deve essere reso a testo, non causare panico.
        let json = r#"{"sections":[{"number":"1","title":"X","content":["riga a","riga b"]},{"number":"2","title":"Y","content":{"text":"annidato"}}]}"#;
        let bytes = render_to_bytes(json);
        let doc = read_zip_part(&bytes, "word/document.xml");
        assert!(doc.contains("riga a"));
        assert!(doc.contains("riga b"));
        assert!(doc.contains("annidato"));
    }

    #[test]
    fn json_non_valido_e_errore() {
        let dir = tempfile::tempdir().unwrap();
        let out = dir.path().join("doc.docx");
        let err = render_document("x", "{non json", out.to_str().unwrap(), "", "T", "P");
        assert!(matches!(err, Err(DocxError::InvalidJson(_))));
        assert!(!out.exists(), "nessun file scritto su input invalido");
    }
}
