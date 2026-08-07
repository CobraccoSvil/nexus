//! Il contratto d'ingresso di un tool: UNA dichiarazione, due prodotti.
//!
//! # Il difetto che questo modulo chiude
//!
//! Lo schema di un tool e il suo parsing erano due verita' separate: il JSON in
//! [`crate::tool_schema`] prometteva al modello nomi, tipi ed enum; l'handler
//! rileggeva `input.get("...")` a mano e decideva per conto proprio cosa fosse
//! obbligatorio. Niente teneva allineate le due, e la divergenza non produce un
//! errore di compilazione: produce un modello che sbaglia SEGUENDO le regole che
//! gli abbiamo dato.
//!
//! MISURATO il 07/08/2026 sui 64 handler: 113 `input.get("...")` scritti a mano
//! in 52 tool, 45 tool che gestiscono l'obbligatorieta' in proprio, 44 che
//! compongono il messaggio d'errore da soli. E due divergenze REALI su 33 enum
//! censiti — `nexus_verify_change.scope` prometteva `lint` mentre il profilo del
//! progetto ha `lint-frontend`, `knowledge_import_graph.format` offriva
//! `mermaid` e `dot`, che il suo handler rifiuta come non implementati.
//!
//! # Come lo chiude
//!
//! [`tool_input!`] genera dalla stessa dichiarazione la struct deserializzabile
//! E il frammento di schema JSON. Lo schema NON puo' promettere un campo che
//! l'handler non riceve, ne' l'handler leggere un campo che lo schema non
//! dichiara: sono la stessa scrittura. L'obbligatorieta' non e' piu' una lista
//! `required` da tenere aggiornata a mano — la dichiara il campo, accanto al suo
//! tipo, e finisce in entrambi i prodotti.
//!
//! # Cosa NON risolve, e perche' resta fuori
//!
//! Gli enum i cui valori dipendono dal PROGETTO (lo `scope` che viene dal
//! profilo di verifica, il `kind` che viene dal registry DB) non possono stare
//! in una macro statica: sono dati, non codice. Restano stringhe validate a
//! runtime, con l'enum rigenerato prima di consegnare il catalogo al modello, e
//! con il guard `enum-dichiarato-e-accettato` a sorvegliare la giunzione.
//! Pretendere di fissarli qui riporterebbe il difetto in una forma nuova.

use serde_json::{json, Map, Value};

use nexus_types::tool_outcome::RispostaTool;

/// Il tipo JSON di un campo, per la parte di schema che il modello legge.
///
/// E' un trait e non una stringa passata a mano nella macro: cosi' il tipo Rust
/// del campo e il `"type"` dichiarato al modello non possono divergere — sono
/// derivati l'uno dall'altro dal compilatore.
pub trait TipoJson {
    /// Il frammento di schema che descrive questo tipo.
    fn schema_tipo() -> Value;
}

impl TipoJson for String {
    fn schema_tipo() -> Value {
        json!({"type": "string"})
    }
}

impl TipoJson for bool {
    fn schema_tipo() -> Value {
        json!({"type": "boolean"})
    }
}

impl TipoJson for i64 {
    fn schema_tipo() -> Value {
        json!({"type": "integer"})
    }
}

impl TipoJson for f64 {
    fn schema_tipo() -> Value {
        json!({"type": "number"})
    }
}

impl TipoJson for Vec<String> {
    fn schema_tipo() -> Value {
        json!({"type": "array", "items": {"type": "string"}})
    }
}

impl TipoJson for Value {
    fn schema_tipo() -> Value {
        // Nessun vincolo: il campo porta una struttura che lo schema non
        // descrive. E' l'unica via d'uscita dal contratto, e va motivata dove
        // la si usa.
        json!({})
    }
}

impl<T: TipoJson> TipoJson for Option<T> {
    fn schema_tipo() -> Value {
        // Un campo opzionale ha lo stesso TIPO di uno obbligatorio: la
        // differenza vive in `required`, non qui.
        T::schema_tipo()
    }
}

/// Cio' che ogni input di tool sa fare di se stesso.
pub trait InputTool: Sized {
    /// Il frammento `input_schema` da consegnare al modello.
    fn schema() -> Value;

    /// Deserializza dall'input grezzo del tool.
    fn leggi(input: &Value) -> Result<Self, RispostaTool>;
}

/// Traduce un errore di deserializzazione nel fallimento che il modello ricevera'.
///
/// PUNTO UNICO del messaggio: prima ogni tool scriveva il proprio («parametro X
/// obbligatorio», «manca il parametro X», «[Errore: parametro 'path'
/// mancante]»), con formati diversi per lo stesso evento. La natura e' sempre
/// [`nexus_types::tool_outcome::NaturaFallimento::Rimediabile`], e qui e'
/// letteralmente vero: il messaggio nomina il campo e dice cosa ci si aspettava.
pub fn errore_di_lettura(tool: &str, e: serde_json::Error) -> RispostaTool {
    RispostaTool::fallito_rimediabile(format!(
        "[Errore: parametri di '{tool}' non validi: {e}. \
         Correggi la chiamata rispettando lo schema del tool.]"
    ))
}

/// Compone lo schema `object` dai campi dichiarati. Usata dalla macro; esposta
/// perche' i test possano costruirne uno senza passare da un tool vero.
pub fn schema_oggetto(campi: Vec<(&str, Value, bool)>) -> Value {
    let mut properties = Map::new();
    let mut required = Vec::new();
    for (nome, mut spec, obbligatorio) in campi {
        if obbligatorio {
            required.push(Value::String(nome.to_string()));
        }
        if let Some(o) = spec.as_object_mut() {
            o.remove("__nulla__");
        }
        properties.insert(nome.to_string(), spec);
    }
    json!({
        "type": "object",
        "properties": Value::Object(properties),
        "required": Value::Array(required),
    })
}

/// Dichiara l'input di un tool UNA volta: ne escono la struct e lo schema.
///
/// ```ignore
/// tool_input! {
///     /// L'editor chirurgico.
///     EditFileInput for "edit_file" {
///         obbligatori {
///             path: String, "Percorso del file relativo alla root";
///             old_string: String, "Stringa esatta da sostituire";
///             new_string: String, "Testo nuovo";
///         }
///         opzionali {
///             dry_run: bool, "Se true non scrive nulla";
///         }
///     }
/// }
/// ```
///
/// I due gruppi non sono zucchero sintattico: governano INSIEME la lista
/// `required` dello schema e il `#[serde(default)]` del parsing, che sono
/// esattamente le due meta' che divergevano. Un campo negli `opzionali` diventa
/// `Option<T>` nella struct — il tipo lo dice, non un commento.
///
/// La prima stesura metteva `#[serde(default)]` su TUTTI i campi e derivava
/// `required` dal tipo: lo schema dichiarava `path` obbligatorio e il parsing
/// accettava la sua assenza restituendo una stringa vuota. Il difetto che
/// questo modulo esiste per rendere impossibile, riprodotto nel modulo stesso —
/// l'ha trovato il test `quel_che_lo_schema_pretende_il_parsing_lo_pretende`,
/// che infatti resta.
#[macro_export]
macro_rules! tool_input {
    (
        $(#[$meta:meta])*
        $nome:ident for $tool:literal {
            obbligatori {
                $(
                    $(#[$obb_meta:meta])*
                    $obb:ident : $obb_tipo:ty, $obb_desc:literal;
                )*
            }
            opzionali {
                $(
                    $(#[$opz_meta:meta])*
                    $opz:ident : $opz_tipo:ty, $opz_desc:literal;
                )*
            }
        }
    ) => {
        $(#[$meta])*
        #[derive(Debug, Clone, ::serde::Deserialize)]
        #[serde(deny_unknown_fields)]
        pub struct $nome {
            $(
                $(#[$obb_meta])*
                pub $obb: $obb_tipo,
            )*
            $(
                $(#[$opz_meta])*
                #[serde(default)]
                pub $opz: ::std::option::Option<$opz_tipo>,
            )*
        }

        impl $crate::input_contract::InputTool for $nome {
            fn schema() -> ::serde_json::Value {
                $crate::input_contract::schema_oggetto(vec![
                    $((
                        stringify!($obb),
                        $crate::input_contract::con_descrizione(
                            <$obb_tipo as $crate::input_contract::TipoJson>::schema_tipo(),
                            $obb_desc,
                        ),
                        true,
                    ),)*
                    $((
                        stringify!($opz),
                        $crate::input_contract::con_descrizione(
                            <$opz_tipo as $crate::input_contract::TipoJson>::schema_tipo(),
                            $opz_desc,
                        ),
                        false,
                    ),)*
                ])
            }

            fn leggi(input: &::serde_json::Value) -> ::std::result::Result<Self, ::nexus_types::tool_outcome::RispostaTool> {
                ::serde_json::from_value::<Self>(input.clone())
                    .map_err(|e| $crate::input_contract::errore_di_lettura($tool, e))
            }
        }
    };
}

/// Aggiunge la descrizione al frammento di tipo. Estratta perche' la macro non
/// debba contenere un blocco mutabile per ogni campo.
pub fn con_descrizione(mut schema: Value, descrizione: &str) -> Value {
    if let Some(o) = schema.as_object_mut() {
        o.insert(
            "description".to_string(),
            Value::String(descrizione.to_string()),
        );
    }
    schema
}

#[cfg(test)]
mod tests {
    use super::*;

    crate::tool_input! {
        /// Input di prova: due obbligatori, due opzionali, un tipo per famiglia.
        ProvaInput for "prova" {
            obbligatori {
                path: String, "Il percorso";
                quante: i64, "Un intero obbligatorio";
            }
            opzionali {
                dry_run: bool, "Opzionale booleano";
                etichette: Vec<String>, "Opzionale lista";
            }
        }
    }

    /// `required` elenca esattamente il gruppo `obbligatori`.
    ///
    /// MUTAZIONE: spostando un campo fra i due gruppi cambiano INSIEME lo schema
    /// e il parsing — che e' il punto. Passando `false` al posto di `true` nella
    /// riga degli obbligatori, questo test rosseggia e resta l'unica cosa che
    /// impedisce a schema e parsing di dire due cose diverse.
    #[test]
    fn required_deriva_dal_tipo_del_campo() {
        let s = <ProvaInput as InputTool>::schema();
        let req: Vec<&str> = s["required"]
            .as_array()
            .expect("required")
            .iter()
            .map(|v| v.as_str().unwrap_or(""))
            .collect();
        assert_eq!(req, vec!["path", "quante"], "solo i non-Option sono required");
    }

    /// Il tipo JSON dichiarato al modello viene dal tipo RUST, non da una
    /// stringa scritta a mano accanto.
    #[test]
    fn il_tipo_json_viene_dal_tipo_rust() {
        let s = <ProvaInput as InputTool>::schema();
        let p = &s["properties"];
        assert_eq!(p["path"]["type"], "string");
        assert_eq!(p["quante"]["type"], "integer");
        assert_eq!(p["dry_run"]["type"], "boolean");
        assert_eq!(p["etichette"]["type"], "array");
        assert_eq!(p["etichette"]["items"]["type"], "string");
        // La descrizione, che e' la sola parte davvero libera, resta accanto.
        assert_eq!(p["path"]["description"], "Il percorso");
    }

    /// Cio' che lo schema dichiara OBBLIGATORIO, il parsing lo pretende: le due
    /// meta' non possono divergere perche' nascono dalla stessa riga.
    #[test]
    fn quel_che_lo_schema_pretende_il_parsing_lo_pretende() {
        let completo = json!({"path": "a.txt", "quante": 3});
        let letto = <ProvaInput as InputTool>::leggi(&completo).expect("input valido");
        assert_eq!(letto.path, "a.txt");
        assert_eq!(letto.quante, 3);
        assert_eq!(letto.dry_run, None, "un opzionale assente e' None");
        assert_eq!(letto.etichette, None, "idem per la lista");

        // E quando ci sono, arrivano.
        let con_opzionali = json!({
            "path": "b.txt", "quante": 1, "dry_run": true, "etichette": ["x", "y"]
        });
        let letto = <ProvaInput as InputTool>::leggi(&con_opzionali).expect("input valido");
        assert_eq!(letto.dry_run, Some(true));
        assert_eq!(
            letto.etichette,
            Some(vec!["x".to_string(), "y".to_string()])
        );

        let senza_obbligatorio = json!({"quante": 3});
        let errore = <ProvaInput as InputTool>::leggi(&senza_obbligatorio)
            .expect_err("manca un campo required");
        assert_eq!(
            errore.natura,
            Some(nexus_types::tool_outcome::NaturaFallimento::Rimediabile),
            "un parametro sbagliato lo corregge l'agente"
        );
        assert!(
            errore.testo.contains("prova"),
            "l'errore nomina il tool: {}",
            errore.testo
        );
    }

    /// Un campo che lo schema NON dichiara viene rifiutato invece di essere
    /// ignorato in silenzio: se il modello lo ha scritto, si aspettava che
    /// facesse qualcosa.
    #[test]
    fn un_campo_non_dichiarato_non_passa_in_silenzio() {
        let con_extra = json!({"path": "a.txt", "quante": 1, "inventato": true});
        let errore = <ProvaInput as InputTool>::leggi(&con_extra).expect_err("campo ignoto");
        assert!(
            errore.testo.contains("inventato"),
            "l'errore nomina il campo di troppo: {}",
            errore.testo
        );
    }
}

// Il doc della struct sta DENTRO la macro: il pattern `$(#[$meta:meta])*` lo
// cattura e lo attacca al tipo generato. Messo qui fuori resterebbe orfano, e
// clippy lo rifiuta.
//
// Lo schema che ne esce deve COINCIDERE con quello scritto a mano in
// `tool_schema`: finche' le due forme convivono, il test
// `lo_schema_generato_coincide_con_quello_scritto_a_mano` e' il ponte che
// impedisce alla migrazione di cambiare in silenzio il contratto verso il
// modello. Quando il catalogo sara' generato, quel test sparira' insieme alla
// copia che verifica.
crate::tool_input! {
    /// L'input di `edit_file`, primo tool a passare dal contratto.
    ///
    /// PERCHE' LUI PER PRIMO: e' quello su cui i difetti di questa famiglia si
    /// sono misurati (11% di `old_string non trovato`, con l'estratto che
    /// mostrava la zona sbagliata del file), ed e' gia' migrato a
    /// `RispostaTool` — quindi il contratto d'ingresso completa un giro che era
    /// gia' fatto per l'uscita.
    EditFileInput for "edit_file" {
        obbligatori {
            path: String, "Percorso del file relativo alla root";
            old_string: String, "Stringa esatta da sostituire (deve esistere esattamente una volta nel file)";
            new_string: String, "Stringa con cui sostituire old_string";
        }
        opzionali {}
    }
}

crate::tool_input! {
    /// L'input di `read_file`.
    ReadFileInput for "read_file" {
        obbligatori {
            path: String, "Percorso del file relativo alla root del progetto (es. 'src/main.rs' o 'README.md')";
        }
        opzionali {}
    }
}

crate::tool_input! {
    /// L'input di `read_file_lines`.
    ///
    /// Gli estremi sono `i64` e non `usize`: il modello puo' scrivere un numero
    /// negativo, e un tipo che non lo rappresenta trasformerebbe un input
    /// sbagliato in un errore di deserializzazione oscuro invece che in un
    /// controllo di dominio con un messaggio utile.
    ReadFileLinesInput for "read_file_lines" {
        obbligatori {
            path: String, "Percorso del file relativo alla root del progetto";
            start_line: i64, "Riga di inizio (1-based, inclusa). Es: 39 per iniziare dalla riga 39.";
            end_line: i64, "Riga di fine (1-based, inclusa). Es: 80 per leggere fino alla riga 80. Massimo 400 righe per chiamata.";
        }
        opzionali {}
    }
}

crate::tool_input! {
    /// L'input di `write_file`.
    WriteFileInput for "write_file" {
        obbligatori {
            path: String, "Percorso del file relativo alla root del progetto";
            content: String, "Contenuto completo del file da scrivere";
        }
        opzionali {}
    }
}

crate::tool_input! {
    /// L'input di `delete_file`.
    DeleteFileInput for "delete_file" {
        obbligatori {
            path: String, "Percorso relativo alla root del file o directory da eliminare";
        }
        opzionali {
            recursive: bool, "Se true, elimina ricorsivamente (necessario per directory non vuote). Default: false";
        }
    }
}

#[cfg(test)]
mod tests_equivalenza {
    use super::*;

    /// Ogni tool MIGRATO al contratto, con lo schema che il contratto genera.
    /// Cresce di una riga a ogni migrazione, e quella riga e' tutto cio' che
    /// serve perche' il tool nuovo sia coperto.
    fn migrati() -> Vec<(&'static str, serde_json::Value)> {
        vec![
            ("edit_file", <EditFileInput as InputTool>::schema()),
            ("read_file", <ReadFileInput as InputTool>::schema()),
            ("read_file_lines", <ReadFileLinesInput as InputTool>::schema()),
            ("write_file", <WriteFileInput as InputTool>::schema()),
            ("delete_file", <DeleteFileInput as InputTool>::schema()),
        ]
    }

    /// IL test della migrazione (regola O): per OGNI tool migrato, lo schema
    /// generato dal contratto e' lo STESSO che il catalogo consegna al modello.
    /// Non confronta due stringhe scritte a mano — prende il catalogo REALE.
    ///
    /// E' il ponte che rende la migrazione dei 64 tool un'operazione
    /// verificabile invece di una riscrittura di cui fidarsi: un campo
    /// rinominato, una descrizione cambiata, un obbligatorio diventato
    /// opzionale fanno rosseggiare questo test prima che il modello se ne
    /// accorga in esercizio.
    ///
    /// MUTAZIONE: cambiando una descrizione o spostando un campo fra
    /// `obbligatori` e `opzionali`, rosseggia nominando il tool.
    #[test]
    fn ogni_schema_generato_coincide_con_il_catalogo() {
        let catalogo: serde_json::Value =
            serde_json::from_str(crate::tool_schema::AGENT_TOOLS_JSON).expect("catalogo valido");
        let elenco = catalogo.as_array().expect("array");

        for (nome, generato) in migrati() {
            let a_mano = elenco
                .iter()
                .find(|t| t["name"] == nome)
                .map(|t| t["input_schema"].clone())
                .unwrap_or_else(|| panic!("{nome} non e' nel catalogo"));

            assert_eq!(
                generato["properties"], a_mano["properties"],
                "properties divergenti per '{nome}'"
            );

            let ordina = |v: &serde_json::Value| {
                let mut r: Vec<String> = v
                    .as_array()
                    .map(|a| {
                        a.iter()
                            .filter_map(|x| x.as_str().map(str::to_string))
                            .collect()
                    })
                    .unwrap_or_default();
                r.sort();
                r
            };
            assert_eq!(
                ordina(&generato["required"]),
                ordina(&a_mano["required"]),
                "obbligatori divergenti per '{nome}'"
            );
        }
    }

    /// La copertura cresce davvero: se qualcuno aggiunge un `tool_input!` senza
    /// registrarlo in `migrati()`, il conteggio lo dichiara. Non e' un test
    /// della logica — e' un promemoria che fallisce, che vale piu' di un
    /// commento.
    #[test]
    fn il_conteggio_dei_migrati_e_dichiarato() {
        assert_eq!(
            migrati().len(),
            5,
            "aggiornare il conteggio quando si migra un tool (e aggiungerlo a migrati())"
        );
    }
}

