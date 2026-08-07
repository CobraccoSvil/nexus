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

impl<T: TipoJson> TipoJson for Vec<T> {
    fn schema_tipo() -> Value {
        json!({"type": "array", "items": T::schema_tipo()})
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

impl TipoJson for Map<String, Value> {
    fn schema_tipo() -> Value {
        // Un oggetto di cui il catalogo dichiara la FORMA (e' un oggetto) ma non
        // il contenuto: il payload di un evento, gli argomenti da inoltrare a un
        // tool MCP esterno. Distinto da [`Value`], che non promette nemmeno
        // questo — e la differenza la legge il modello, non noi.
        json!({"type": "object"})
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
/// La descrizione puo' stare su piu' letterali adiacenti, concatenati senza
/// separatore: le descrizioni del catalogo arrivano a 325 caratteri, e su una
/// riga sola sforavano il gate delle righe lunghe di `quality-scan`. Lo spazio
/// fra un pezzo e il successivo va scritto DENTRO il pezzo — se manca, il testo
/// che il modello legge cambia, e il test di equivalenza col catalogo lo dice.
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
                    $obb:ident : $obb_tipo:ty, $($obb_desc:literal)+;
                )*
            }
            opzionali {
                $(
                    $(#[$opz_meta:meta])*
                    $opz:ident : $opz_tipo:ty, $($opz_desc:literal)+;
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
                            ::std::concat!($($obb_desc),+),
                        ),
                        true,
                    ),)*
                    $((
                        stringify!($opz),
                        $crate::input_contract::con_descrizione(
                            <$opz_tipo as $crate::input_contract::TipoJson>::schema_tipo(),
                            ::std::concat!($($opz_desc),+),
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

/// Dichiara UNA volta i valori ammessi di un campo: ne escono il tipo Rust che
/// li rappresenta e l'`enum` che lo schema promette al modello.
///
/// ```ignore
/// tool_enum! {
///     /// In che forma restituire il contenuto.
///     Encoding {
///         Auto => "auto";
///         Text => "text";
///         Base64 => "base64";
///     }
/// }
/// ```
///
/// # Perche' un tipo e non una `String`
///
/// Era la sola famiglia di campi su cui il catalogo prometteva PIU' di quanto
/// l'handler pretendesse: lo schema dichiarava quattro valori, il parsing
/// accettava qualunque stringa e il controllo — dove c'era — era un `match` con
/// un ramo `_` scritto a mano. Le due divergenze REALI misurate il 07/08/2026
/// (`nexus_verify_change.scope` che prometteva `lint` contro il `lint-frontend`
/// del profilo, `knowledge_import_graph.format` che offriva `mermaid` e `dot`
/// che l'handler rifiuta) stanno entrambe qui.
///
/// Col tipo, un valore fuori elenco non arriva all'handler: lo ferma la
/// deserializzazione, e il messaggio che serde compone elenca i valori ammessi —
/// quindi il fallimento e' rimediabile ED e' accompagnato da cio' che serve per
/// rimediare, senza che nessuno scriva quell'elenco una seconda volta.
///
/// # Perche' i valori restano scritti accanto alle varianti
///
/// `rename_all = "snake_case"` avrebbe dedotto la stringa dal nome della
/// variante, ma i due non coincidono sempre (`RetryOk` non da' `retry_ok` per
/// tutte le convenzioni, e `1024x1024` non e' un identificatore affatto). La
/// stringa e' il contratto col modello: si dichiara, non si deriva.
#[macro_export]
macro_rules! tool_enum {
    (
        $(#[$meta:meta])*
        $nome:ident {
            $(
                $(#[$var_meta:meta])*
                $variante:ident => $valore:literal;
            )+
        }
    ) => {
        $(#[$meta])*
        #[derive(Debug, Clone, Copy, PartialEq, Eq, ::serde::Serialize, ::serde::Deserialize)]
        pub enum $nome {
            $(
                $(#[$var_meta])*
                #[serde(rename = $valore)]
                $variante,
            )+
        }

        impl $nome {
            /// Il valore canonico sul wire (regola N): quello che il modello
            /// scrive e quello che finisce in DB o in un payload.
            ///
            /// Non e' una `impl Display`: un `to_string()` che qualcuno rilegga
            /// e' un protocollo travestito (regola Q). Questo e' un accessore, e
            /// chi lo chiama sta serializzando, non raccontando.
            pub fn come_stringa(self) -> &'static str {
                match self {
                    $(Self::$variante => $valore,)+
                }
            }

            /// I valori ammessi, nell'ordine in cui il catalogo li dichiara.
            pub fn valori() -> &'static [&'static str] {
                &[$($valore,)+]
            }
        }

        impl $crate::input_contract::TipoJson for $nome {
            fn schema_tipo() -> ::serde_json::Value {
                ::serde_json::json!({
                    "type": "string",
                    "enum": <$nome>::valori(),
                })
            }
        }
    };
}

/// Aggiunge la descrizione al frammento di tipo. Estratta perche' la macro non
/// debba contenere un blocco mutabile per ogni campo.
///
/// Una descrizione VUOTA non diventa `"description": ""`: la chiave resta
/// assente. Non e' cosmesi — sei campi del catalogo non hanno descrizione
/// (`attachment_id` dei tool di estrazione), e una chiave vuota li avrebbe fatti
/// divergere dal catalogo, cioe' avrebbe cambiato il contratto verso il modello
/// dentro un lavoro che deve lasciarlo identico. Il vuoto qui significa «il
/// catalogo non la dichiara», e scriverle e' un lavoro a se'.
pub fn con_descrizione(mut schema: Value, descrizione: &str) -> Value {
    if descrizione.is_empty() {
        return schema;
    }
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

    crate::tool_enum! {
        /// Enum di prova: valori snake_case, uno con una cifra.
        Forma {
            Auto => "auto";
            Testo => "text";
            RetryOk => "retry_ok";
        }
    }

    crate::tool_input! {
        /// Input di prova che usa l'enum: singolo e dentro una lista.
        #[allow(dead_code)]
        ConEnum for "con_enum" {
            obbligatori {
                forma: Forma, "La forma";
            }
            opzionali {
                forme: Vec<Forma>, "Le forme ammesse";
                libero: ::serde_json::Map<String, ::serde_json::Value>, "Oggetto senza forma dichiarata";
                senza_descrizione: String, "";
            }
        }
    }

    /// Un valore fuori elenco non arriva all'handler: lo ferma il parsing, e il
    /// messaggio porta i valori ammessi senza che nessuno li riscriva.
    ///
    /// MUTAZIONE: togliendo `#[serde(rename = $valore)]` dalla macro, il valore
    /// `"text"` non viene piu' riconosciuto (serde cercherebbe `"Testo"`) e la
    /// prima meta' del test rosseggia.
    #[test]
    fn un_valore_fuori_elenco_non_arriva_all_handler() {
        let valido = json!({"forma": "retry_ok"});
        let letto = <ConEnum as InputTool>::leggi(&valido).expect("valore ammesso");
        assert_eq!(letto.forma, Forma::RetryOk);
        assert_eq!(letto.forma.come_stringa(), "retry_ok", "il wire e' il valore");

        let fuori = json!({"forma": "urgentissimo"});
        let errore = <ConEnum as InputTool>::leggi(&fuori).expect_err("valore non ammesso");
        assert_eq!(
            errore.natura,
            Some(nexus_types::tool_outcome::NaturaFallimento::Rimediabile),
            "scegliere un altro valore e' cosa che l'agente puo' fare"
        );
        for ammesso in Forma::valori() {
            assert!(
                errore.testo.contains(ammesso),
                "l'errore elenca i valori ammessi, manca '{ammesso}': {}",
                errore.testo
            );
        }
    }

    /// Il vincolo sopravvive DENTRO una lista.
    ///
    /// E' il difetto misurato sul catalogo reale: `knowledge_get_subgraph.rel_types`
    /// e `nexus_search_semantic.source_kinds` dichiarano l'enum negli `items`, e
    /// un `Vec<String>` lo avrebbe perso — cioe' avrebbe tolto al modello un
    /// vincolo che oggi ha.
    ///
    /// MUTAZIONE: riportando `Vec<T>` a un impl fisso su `Vec<String>`, gli
    /// `items` perdono la chiave `enum` e questo test rosseggia.
    #[test]
    fn una_lista_di_enum_conserva_il_vincolo_negli_items() {
        let s = <ConEnum as InputTool>::schema();
        let items = &s["properties"]["forme"]["items"];
        assert_eq!(items["type"], "string");
        assert_eq!(
            items["enum"],
            json!(["auto", "text", "retry_ok"]),
            "gli items portano l'enum, nell'ordine dichiarato"
        );

        let letto = <ConEnum as InputTool>::leggi(&json!({"forma": "auto", "forme": ["text"]}))
            .expect("lista valida");
        assert_eq!(letto.forme, Some(vec![Forma::Testo]));
        <ConEnum as InputTool>::leggi(&json!({"forma": "auto", "forme": ["inventato"]}))
            .expect_err("un valore fuori elenco non passa nemmeno dentro una lista");
    }

    /// Le due forme dell'ignoto non si confondono: un oggetto di cui il catalogo
    /// dichiara almeno il TIPO, e un campo su cui non promette nulla.
    #[test]
    fn oggetto_libero_e_assenza_di_vincolo_restano_distinti() {
        let s = <ConEnum as InputTool>::schema();
        assert_eq!(s["properties"]["libero"]["type"], "object");
        assert_eq!(
            <serde_json::Value as TipoJson>::schema_tipo(),
            json!({}),
            "Value non promette nemmeno di essere un oggetto"
        );
    }

    /// Una descrizione assente nel catalogo resta assente: non diventa una
    /// chiave vuota che il modello dovrebbe leggere come informazione.
    ///
    /// MUTAZIONE: togliendo il ritorno anticipato in `con_descrizione`, compare
    /// `"description": ""` e questo test rosseggia — con esso i sei tool di
    /// estrazione allegati, che nel catalogo non la dichiarano.
    #[test]
    fn una_descrizione_assente_non_diventa_una_chiave_vuota() {
        let s = <ConEnum as InputTool>::schema();
        let campo = s["properties"]["senza_descrizione"]
            .as_object()
            .expect("oggetto");
        assert!(
            !campo.contains_key("description"),
            "la chiave non c'e' affatto: {campo:?}"
        );
        assert_eq!(
            s["properties"]["forma"]["description"], "La forma",
            "quella dichiarata resta"
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
