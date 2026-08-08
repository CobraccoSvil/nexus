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

impl TipoJson for nexus_types::severity::Severity {
    fn schema_tipo() -> Value {
        // Il vocabolario NON si riscrive qui: viene dal punto unico
        // (`nexus-types::severity`), che e' lo stesso tipo che i panel usano per
        // decidere il veto in minoranza. Un `tool_enum!` con gli stessi tre
        // valori sarebbe stato un gemello, e nessun compilatore avrebbe
        // obbligato i due a restare allineati — che e' la ragione per cui quel
        // modulo e' migrato in `nexus-types`.
        json!({
            "type": "string",
            "enum": nexus_types::severity::Severity::VALORI,
        })
    }
}

impl TipoJson for nexus_types::source_kind::SourceKind {
    fn schema_tipo() -> Value {
        // Come per `Severity`: il vocabolario viene dal punto unico, non da un
        // elenco riscritto qui. E' il caso in cui la duplicazione aveva gia'
        // fatto danno — il catalogo prometteva 5 valori, `SourceKind::parse` ne
        // accettava 8, e le tre sorgenti in piu' erano irraggiungibili per chi
        // doveva chiederle.
        json!({
            "type": "string",
            "enum": nexus_types::source_kind::SourceKind::VALORI,
        })
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
///
/// Un `required` VUOTO non viene scritto. In JSON Schema le due forme dicono la
/// stessa cosa, ma il catalogo sceglie l'assenza (20 tool su 24 senza campi
/// obbligatori la omettono, e la omettono ANCHE gli oggetti annidati come il
/// `viewport` di `nexus_visual_compare`) — e per un oggetto annidato il
/// confronto col catalogo e' profondo, quindi una chiave in piu' li' e' una
/// divergenza vera, non una sfumatura di stile.
pub fn schema_oggetto(campi: Vec<(&str, Value, bool)>) -> Value {
    let mut properties = Map::new();
    let mut required = Vec::new();
    for (nome, spec, obbligatorio) in campi {
        // `stringify!` di un identificatore grezzo conserva il prefisso: il
        // campo `type` di `nexus_todo_write` si scrive `r#type` in Rust perche'
        // e' parola riservata, e senza questa riga lo schema avrebbe promesso al
        // modello una property chiamata `r#type`. Serde il prefisso lo toglie
        // gia' per conto suo, quindi il parsing era corretto e solo lo schema
        // avrebbe mentito — la meta' del contratto che nessun compilatore
        // controlla.
        let nome = nome.strip_prefix("r#").unwrap_or(nome);
        if obbligatorio {
            required.push(Value::String(nome.to_string()));
        }
        properties.insert(nome.to_string(), spec);
    }
    let mut schema = Map::new();
    schema.insert("type".to_string(), Value::String("object".to_string()));
    schema.insert("properties".to_string(), Value::Object(properties));
    if !required.is_empty() {
        schema.insert("required".to_string(), Value::Array(required));
    }
    Value::Object(schema)
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
        $crate::tool_object! {
            $(#[$meta])*
            $nome {
                obbligatori {
                    $(
                        $(#[$obb_meta])*
                        $obb: $obb_tipo, $($obb_desc)+;
                    )*
                }
                opzionali {
                    $(
                        $(#[$opz_meta])*
                        $opz: $opz_tipo, $($opz_desc)+;
                    )*
                }
            }
        }

        impl $crate::input_contract::InputTool for $nome {
            fn schema() -> ::serde_json::Value {
                // Lo schema dell'input E' lo schema dell'oggetto: una seconda
                // composizione qui potrebbe divergere da quella che il campo
                // annidato produce, e sarebbe di nuovo due verita' per la stessa
                // domanda.
                <Self as $crate::input_contract::TipoJson>::schema_tipo()
            }

            fn leggi(input: &::serde_json::Value) -> ::std::result::Result<Self, ::nexus_types::tool_outcome::RispostaTool> {
                ::serde_json::from_value::<Self>(input.clone())
                    .map_err(|e| $crate::input_contract::errore_di_lettura($tool, e))
            }
        }
    };
}

/// Dichiara un oggetto ANNIDATO: la stessa forma di [`tool_input!`], senza il
/// nome di un tool perche' non e' l'input di nessuno.
///
/// Serve dove il catalogo descrive la forma di una struttura dentro un campo —
/// i `files` di `batch_analyze_code`, il `viewport` di `nexus_visual_compare`,
/// gli `endpoints` di `task_complete`. Il tipo che ne esce implementa
/// [`TipoJson`], quindi si usa come tipo di campo esattamente come `String`, e
/// `Vec<T>` lo porta dentro un array senza altro lavoro.
///
/// [`tool_input!`] la CHIAMA invece di ripetere il corpo: struct e schema
/// nascono da una sola scrittura, che e' l'intero punto di questo modulo.
#[macro_export]
macro_rules! tool_object {
    (
        $(#[$meta:meta])*
        $nome:ident {
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

        impl $crate::input_contract::TipoJson for $nome {
            fn schema_tipo() -> ::serde_json::Value {
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

/// Dichiara un campo i cui valori ammessi li fissa il RUNTIME, non il codice.
///
/// ```ignore
/// tool_enum_dinamico! {
///     /// Lo scope di verifica: i valori veri vengono dal profilo del progetto.
///     ScopeVerifica {
///         seed { "quick"; "full"; "typecheck"; }
///     }
/// }
/// ```
///
/// # Perche' non e' un [`tool_enum!`]
///
/// Un enum Rust vincola il PARSING, e qui il vincolo sarebbe sbagliato: i
/// valori veri di `nexus_verify_change.scope` vengono dal profilo del progetto
/// (`lint-frontend`, `typecheck-backend`), quelli di `dispatch_subagent.kind`
/// dal registry DB. Un enum statico rifiuterebbe alla deserializzazione proprio
/// i valori che il catalogo — rigenerato prima di consegnarlo al modello — gli
/// ha promesso. Sarebbe il difetto misurato il 07/08/2026 (`lint` promesso e
/// `invalid_scope` in risposta) riprodotto un livello piu' in basso, dove
/// nessun messaggio d'errore lo spiegherebbe.
///
/// Il tipo che ne esce e' quindi una stringa nel parsing, e DICE di esserlo. Il
/// controllo dei valori resta dove sono i dati per farlo, cioe' a runtime.
///
/// # Perche' il seed sta comunque qui
///
/// `agent_turn_setup::apply_verify_scope_enum` SOSTITUISCE l'array `enum` dello
/// schema (`pointer_mut(".../scope/enum")`): senza un array da sostituire non
/// aggancia nulla, e il modello resterebbe senza vincolo. Il seed non e' un
/// residuo da togliere — e' cio' che rende possibile la sostituzione, e vale
/// anche come ripiego quando il profilo e' vuoto (progetto nuovo, DB
/// irraggiungibile). Dichiararlo qui lo mette accanto al campo che governa,
/// invece che dentro un letterale JSON dall'altra parte del crate.
#[macro_export]
macro_rules! tool_enum_dinamico {
    (
        $(#[$meta:meta])*
        $nome:ident {
            seed { $($valore:literal;)+ }
        }
    ) => {
        $(#[$meta])*
        #[derive(Debug, Clone, PartialEq, Eq, ::serde::Serialize, ::serde::Deserialize)]
        #[serde(transparent)]
        pub struct $nome(pub ::std::string::String);

        impl $nome {
            /// I valori del SEED: quelli che lo schema porta finche' il runtime
            /// non li sostituisce coi veri. NON sono l'insieme ammesso.
            pub fn seed() -> &'static [&'static str] {
                &[$($valore,)+]
            }

            /// Il valore come lo ha scritto il modello. Chi lo riceve lo valida
            /// contro la fonte vera (profilo del progetto, registry DB).
            pub fn come_stringa(&self) -> &str {
                &self.0
            }
        }

        impl $crate::input_contract::TipoJson for $nome {
            fn schema_tipo() -> ::serde_json::Value {
                ::serde_json::json!({
                    "type": "string",
                    "enum": <$nome>::seed(),
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

    crate::tool_object! {
        /// Oggetto di prova annidato, con un campo dal nome riservato in Rust.
        #[allow(dead_code)]
        Criterio {
            obbligatori {
                r#type: String, "Il tipo del criterio";
            }
            opzionali {
                soglia: i64, "Una soglia";
            }
        }
    }

    crate::tool_input! {
        /// Input di prova che annida: un oggetto singolo e una lista di oggetti.
        #[allow(dead_code)]
        ConAnnidati for "con_annidati" {
            obbligatori {
                criterio: Criterio, "Il criterio principale";
            }
            opzionali {
                altri: Vec<Criterio>, "Gli altri criteri";
            }
        }
    }

    /// Un oggetto dichiarato con `tool_object!` e' un tipo come gli altri: sta
    /// in un campo, sta dentro un `Vec`, e lo schema che ne esce e' lo schema
    /// oggetto — non una copia scritta a parte che potrebbe divergerne.
    ///
    /// MUTAZIONE: se `InputTool::schema` tornasse a comporre lo schema per
    /// conto proprio invece di delegare a `TipoJson`, le due meta' potrebbero
    /// dire cose diverse e questo test smetterebbe di provarlo.
    #[test]
    fn un_oggetto_annidato_e_un_tipo_come_gli_altri() {
        let s = <ConAnnidati as InputTool>::schema();
        let dentro = &s["properties"]["criterio"];
        assert_eq!(dentro["type"], "object");
        assert_eq!(dentro["properties"]["soglia"]["type"], "integer");
        assert_eq!(dentro["required"], json!(["type"]));
        // Stessa forma, ma la DESCRIZIONE appartiene al campo: su un array si
        // attacca all'array e non ai suoi elementi. E' cio' che fa il catalogo
        // (i `todos` di `nexus_todo_write` la portano sull'array), quindi il
        // confronto e' sulle properties.
        let items = &s["properties"]["altri"]["items"];
        assert_eq!(items["properties"], dentro["properties"]);
        assert_eq!(items["required"], dentro["required"]);
        assert!(
            items.get("description").is_none(),
            "la descrizione resta sull'array: {items}"
        );

        let letto = <ConAnnidati as InputTool>::leggi(&json!({
            "criterio": {"type": "http", "soglia": 200},
            "altri": [{"type": "file_exists"}]
        }))
        .expect("annidato valido");
        assert_eq!(letto.criterio.r#type, "http");
        assert_eq!(letto.criterio.soglia, Some(200));
        assert_eq!(letto.altri.as_ref().map(Vec::len), Some(1));

        <ConAnnidati as InputTool>::leggi(&json!({"criterio": {"soglia": 1}}))
            .expect_err("manca un obbligatorio dell'annidato");
    }

    /// Un campo che in Rust si scrive `r#type` resta `type` verso il modello.
    ///
    /// MUTAZIONE: togliendo lo `strip_prefix("r#")` da `schema_oggetto`, lo
    /// schema promette una property `r#type` che il modello non scrivera' mai —
    /// e il parsing continuerebbe a funzionare, perche' serde il prefisso lo
    /// toglie da solo. Cioe' esattamente il tipo di divergenza silenziosa che
    /// questo modulo esiste per rendere impossibile.
    #[test]
    fn un_nome_riservato_in_rust_resta_il_nome_del_catalogo() {
        let s = <Criterio as TipoJson>::schema_tipo();
        let props = s["properties"].as_object().expect("oggetto");
        assert!(props.contains_key("type"), "la property e' 'type': {props:?}");
        assert!(!props.contains_key("r#type"), "il prefisso Rust non esce: {props:?}");
        assert_eq!(s["required"], json!(["type"]));
    }

    /// Un `required` vuoto non compare affatto: e' la forma che il catalogo
    /// usa, e per un oggetto ANNIDATO il confronto e' profondo — quindi una
    /// chiave in piu' li' e' una divergenza vera.
    ///
    /// MUTAZIONE: riscrivendo `required` sempre, il `viewport` di
    /// `nexus_visual_compare` smette di coincidere col catalogo e il test di
    /// equivalenza rosseggia.
    #[test]
    fn un_required_vuoto_non_si_scrive() {
        let solo_opzionali = schema_oggetto(vec![("a", json!({"type": "string"}), false)]);
        assert!(
            solo_opzionali.get("required").is_none(),
            "nessun obbligatorio, nessuna chiave: {solo_opzionali}"
        );
        let con_obbligatorio = schema_oggetto(vec![("a", json!({"type": "string"}), true)]);
        assert_eq!(con_obbligatorio["required"], json!(["a"]));
    }

    crate::tool_enum_dinamico! {
        /// Tipo di prova a valori dinamici: il seed e' un ripiego, non l'insieme
        /// ammesso.
        ScopeDiProva {
            seed { "quick"; "full"; "lint"; }
        }
    }

    crate::tool_input! {
        /// Input di prova con un campo a valori dinamici.
        #[allow(dead_code)]
        ConDinamico for "con_dinamico" {
            obbligatori {
                scope: ScopeDiProva, "Lo scope";
            }
            opzionali {}
        }
    }

    /// Un campo a valori dinamici PORTA il seed nello schema ma NON lo impone al
    /// parsing: i valori veri arrivano dal profilo del progetto, e lo schema che
    /// il modello legge e' gia' stato riscritto con quelli.
    ///
    /// MUTAZIONE: dichiarando `scope` con `tool_enum!` invece che con
    /// `tool_enum_dinamico!`, `lint-frontend` viene rifiutato dalla
    /// deserializzazione — cioe' l'agente riceve un errore per aver usato uno
    /// dei valori che il catalogo gli aveva appena promesso, che e' il difetto
    /// misurato il 07/08/2026 riprodotto un livello piu' in basso.
    #[test]
    fn un_valore_dal_progetto_non_lo_ferma_il_seed() {
        let s = <ConDinamico as InputTool>::schema();
        assert_eq!(
            s["properties"]["scope"]["enum"],
            json!(["quick", "full", "lint"]),
            "il seed sta nello schema: e' cio' che apply_verify_scope_enum sostituisce"
        );

        // Il valore vero di un progetto reale: fuori dal seed, e legittimo.
        let letto = <ConDinamico as InputTool>::leggi(&json!({"scope": "lint-frontend"}))
            .expect("il seed non e' un vincolo di parsing");
        assert_eq!(letto.scope.come_stringa(), "lint-frontend");

        // E il seed resta interrogabile per chi deve ripiegarci.
        assert!(ScopeDiProva::seed().contains(&"quick"));
    }

    /// Un vocabolario che ha gia' un punto unico non viene riscritto: lo schema
    /// lo DERIVA dal tipo condiviso.
    ///
    /// MUTAZIONE: ricopiando i valori a mano dentro `schema_tipo`, questo test
    /// resta verde — ma il giorno in cui il punto unico ne aggiunge uno, la
    /// copia non lo segue. E' la ragione per cui l'asserzione confronta con la
    /// costante del punto unico invece che con un elenco letterale.
    #[test]
    fn i_vocabolari_condivisi_vengono_dal_punto_unico() {
        use nexus_types::{severity::Severity, source_kind::SourceKind};

        let s = <Severity as TipoJson>::schema_tipo();
        assert_eq!(s["type"], "string");
        assert_eq!(s["enum"], json!(Severity::VALORI));

        let k = <SourceKind as TipoJson>::schema_tipo();
        assert_eq!(k["enum"], json!(SourceKind::VALORI));
        assert_eq!(
            k["enum"].as_array().map(Vec::len),
            Some(8),
            "tutte le sorgenti che SourceKind::parse accetta, non le 5 storiche"
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
