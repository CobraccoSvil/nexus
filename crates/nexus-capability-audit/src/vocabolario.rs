//! «Di questa capability dichiarata: CHI la legge, di CHI e' la proprieta', e
//! con quale PROVA si accerta?»
//!
//! Punto unico (regola L) del vocabolario delle colonne di
//! `v_model_capabilities`, la vista che ADR 0024 dichiara fonte unica della
//! capability. Le tre domande sono ortogonali e vanno tenute distinte, perche'
//! ognuna vieta una mossa diversa:
//!
//!   - **chi la legge** decide se un valore sbagliato ha conseguenze. Una
//!     colonna che nessuno legge non e' «un dato da automatizzare»: e' un dato
//!     da togliere o da collegare, e costruirle attorno un ciclo di verifica
//!     significherebbe accertare cio' che nessuno usa.
//!   - **di chi e' la proprieta'** decide se «accertare» voglia dire qualcosa.
//!     `max_context_tokens` e' del fornitore e si puo' sbagliare;
//!     `history_keep_recent_messages` e' una NOSTRA politica, e non esiste
//!     esperimento che dica se e' «vera».
//!   - **con quale prova** decide QUALE strada di automazione e' percorribile,
//!     e per due colonne la risposta e' che oggi non lo e' nessuna.
//!
//! MISURATO il 10/08/2026 sul repo e sul META vivo. La vista espone 32 colonne;
//! l'intero codice Rust vi esegue TRE `SELECT`, e leggono tre colonne:
//! `tool_choice_style` (`mcp-core/src/capability.rs:88`),
//! `agentic_thinking_policy` (`capability.rs:175`) e `tool_result_max_chars`
//! (`native_engine.rs:2413`). Delle 22 meccaniche che arrivano da
//! `nexus_provider_capabilities` ne restano percio' 20 che nessun `SELECT`
//! legge, in nessun crate: 166 righe portano valori che nessuno consulta. I
//! sette flag semantici che la vista deriva da `ai_price_catalog` sono invece
//! letti eccome — ma
//! DIRETTAMENTE dalla tabella (`orchestrator/model_selection.rs:407`,
//! `orchestrator/core.rs:607` e `:1965`), scavalcando la vista che li espone.
//!
//! Percio' «e' nella vista» non significa «e' in uso», ed e' la distinzione che
//! il vocabolario porta: senza, chiunque legga `v_model_capabilities` deve
//! dedurre a naso quali colonne contino, e la deduzione piu' naturale — «ci
//! sono tutte, quindi servono tutte» — e' falsa per 20 colonne su 32.
//!
//! CHE COSA IMPEDISCE DI MARCIRE. Il vocabolario non e' un commento: e' un
//! elenco con un guard che lo confronta con le colonne che le migrazioni vere
//! producono (`vocabolario_copre_la_vista_reale`). Una colonna aggiunta domani
//! rende ROSSO il test finche' qualcuno non ne dichiara le tre risposte — cioe'
//! il passo che oggi manca all'onboarding non e' «scrivere la riga», e'
//! «dichiarare che cosa quella riga significhi e chi se ne accorgera' se e'
//! falsa».

/// Chi legge la colonna. Non e' una sfumatura di stile: e' cio' che distingue
/// un dato sbagliato da un dato inerte, e i due hanno rimedi opposti.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Lettura {
    /// Letta attraverso la VISTA, cioe' dal punto che ADR 0024 dichiara unico.
    /// Il `punto` e' il file:riga del `SELECT`, non una descrizione: chi
    /// verifica deve poterci arrivare.
    ViaVista { punto: &'static str },
    /// Letta, ma dalla TABELLA che alimenta la vista invece che dalla vista.
    /// Non e' un difetto di per se' — i flag semantici vivono in
    /// `ai_price_catalog` e li' vengono filtrati in SQL dalla selezione modelli
    /// — ma significa che la vista NON e' il punto per cui passa quel valore, e
    /// un consumatore nuovo che si fidasse della vista leggerebbe la stessa cosa
    /// per un'altra strada.
    DallaTabella { punto: &'static str },
    /// Nessun `SELECT` in nessun crate. La colonna esiste, ha valori, e non ha
    /// lettori: un valore sbagliato qui non produce alcun sintomo, e quindi non
    /// verra' mai scoperto dall'esercizio.
    NessunLettore,
}

impl Lettura {
    /// Identificatore canonico sul wire e nei report (regola N).
    pub fn wire(&self) -> &'static str {
        match self {
            Lettura::ViaVista { .. } => "via_vista",
            Lettura::DallaTabella { .. } => "dalla_tabella",
            Lettura::NessunLettore => "nessun_lettore",
        }
    }

    /// `true` se esiste un consumatore, per qualunque strada. E' il predicato su
    /// cui si decide se valga la pena accertare la colonna: su una colonna senza
    /// lettori un ciclo di verifica spenderebbe chiamate al fornitore per
    /// correggere un dato che non cambia il comportamento di nulla.
    pub fn ha_un_lettore(&self) -> bool {
        !matches!(self, Lettura::NessunLettore)
    }
}

/// Di chi e' la proprieta' che la colonna descrive. Decide se la domanda
/// «e' vera?» sia ponibile.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Proprieta {
    /// Identifica la riga. Non e' una capability e non si accerta.
    Identita,
    /// Proprieta' del FORNITORE: esiste una risposta giusta indipendente da noi,
    /// quindi la dichiarazione puo' essere falsa e un esperimento puo' dirlo.
    DelFornitore,
    /// NOSTRA politica travestita da capability: soglie, budget, tagli. Nessun
    /// esperimento puo' dire se sia «vera», perche' non c'e' nulla fuori da noi
    /// a cui corrisponda. Automatizzarne la scrittura sarebbe far decidere a un
    /// probe una scelta che e' di configurazione (regola G: quel posto e'
    /// `settings`, non una tabella di capability).
    NostraPolicy,
}

impl Proprieta {
    /// Identificatore canonico (regola N).
    pub fn wire(&self) -> &'static str {
        match self {
            Proprieta::Identita => "identita",
            Proprieta::DelFornitore => "del_fornitore",
            Proprieta::NostraPolicy => "nostra_policy",
        }
    }
}

/// Con quale prova la colonna si accerterebbe. E' il campo che dice quale delle
/// strade di automazione e' percorribile PER QUELLA COLONNA — la domanda non ha
/// una risposta sola per tutta la tabella, ed e' il motivo per cui «automatizzare
/// le capability» non e' un lavoro solo.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Accertamento {
    /// Un segnale strutturato gia' prodotto dall'esercizio la conferma o la
    /// smentisce, e qualcuno lo scrive gia'. E' la sola classe su cui una
    /// scrittura automatica poggerebbe su dati esistenti.
    DallEsercizio { segnale: &'static str },
    /// La prova esiste solo se si MANDA il parametro in questione: l'esito di
    /// una chiamata che non lo ha mandato non dice nulla sul suo valore. E' il
    /// caso di `tool_choice_style`, e la conseguenza e' che l'osservazione deve
    /// registrare lo STIMOLO e non il solo esito (vedi il doc del crate).
    SoloInviandoIlParametro { motivo: &'static str },
    /// Non accertabile: o la proprieta' e' nostra, o non esiste una risposta
    /// osservabile. Dichiararlo evita che qualcuno progetti un probe per una
    /// domanda senza risposta.
    NonAccertabile { motivo: &'static str },
}

impl Accertamento {
    /// Identificatore canonico (regola N).
    pub fn wire(&self) -> &'static str {
        match self {
            Accertamento::DallEsercizio { .. } => "dall_esercizio",
            Accertamento::SoloInviandoIlParametro { .. } => "solo_inviando_il_parametro",
            Accertamento::NonAccertabile { .. } => "non_accertabile",
        }
    }
}

/// Una colonna della vista con le tre risposte. `nome` e' il nome ESATTO della
/// colonna in `v_model_capabilities`: e' la chiave con cui il guard confronta il
/// vocabolario con lo schema reale.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ColonnaCapability {
    pub nome: &'static str,
    pub lettura: Lettura,
    pub proprieta: Proprieta,
    pub accertamento: Accertamento,
}

/// Le due colonne che identificano la riga: la loro lettura e' la clausola
/// `WHERE` delle tre query, non un accesso a un dato.
const PUNTO_IDENTITA: Lettura = Lettura::ViaVista {
    punto: "mcp-core/src/capability.rs:89 (WHERE)",
};
const NON_E_CAPABILITY: Accertamento = Accertamento::NonAccertabile {
    motivo: "identifica la riga",
};

/// I quattro flag media entrano tutti dalla stessa mappa capability->colonna
/// della selezione modelli: un punto solo, quindi una costante sola.
const PUNTO_MEDIA: Lettura = Lettura::DallaTabella {
    punto: "mcp-core/src/orchestrator/model_selection.rs:276 (mappa capability->colonna)",
};

/// Motivo ricorrente: una soglia nostra non ha un valore «vero» da scoprire.
const POLICY_NOSTRA: Accertamento = Accertamento::NonAccertabile {
    motivo: "soglia decisa da noi: non esiste una risposta del fornitore a cui corrisponda",
};

/// Motivo ricorrente: il dialetto si accerterebbe solo mandando una richiesta in
/// quel dialetto, cioe' facendo proprio la cosa di cui si dubita.
const DIALETTO_DA_INVIARE: Accertamento = Accertamento::SoloInviandoIlParametro {
    motivo: "il dialetto si prova solo emettendolo: una richiesta che non lo usa non lo smentisce",
};

/// Il vocabolario. Ordine = quello della vista, cosi' che il confronto col
/// guard sia leggibile quando fallisce.
///
/// I `punto` sono file:riga misurati il 10/08/2026. Se si spostano, il guard
/// testuale di `check-single-source.sh` non se ne accorge — quello verifica che
/// il vocabolario esista e sia qui; la corrispondenza col codice la tiene chi
/// modifica quel codice, che e' l'unico che sappia di averlo fatto.
pub const COLONNE: &[ColonnaCapability] = &[
    ColonnaCapability {
        nome: "provider",
        lettura: PUNTO_IDENTITA,
        proprieta: Proprieta::Identita,
        accertamento: NON_E_CAPABILITY,
    },
    ColonnaCapability {
        nome: "model",
        lettura: PUNTO_IDENTITA,
        proprieta: Proprieta::Identita,
        accertamento: NON_E_CAPABILITY,
    },
    // --- flag semantici derivati da ai_price_catalog -------------------------
    ColonnaCapability {
        nome: "tool_use",
        // Letto dalla TABELLA: la selezione modelli lo filtra in SQL.
        lettura: Lettura::DallaTabella {
            punto: "mcp-core/src/orchestrator/model_selection.rs:407",
        },
        proprieta: Proprieta::DelFornitore,
        // E' l'UNICA capability che ha gia' un ciclo di verifica completo:
        // due scrittori automatici, una soglia, un lock umano, un reason.
        accertamento: Accertamento::DallEsercizio {
            segnale: "ai_model_health_history.error_kind LIKE 'tool_probe:%' + tool_capability.rs",
        },
    },
    ColonnaCapability {
        nome: "vision",
        lettura: Lettura::DallaTabella {
            punto: "mcp-core/src/orchestrator/core.rs:607",
        },
        proprieta: Proprieta::DelFornitore,
        accertamento: Accertamento::SoloInviandoIlParametro {
            motivo: "si prova mandando un'immagine: nessun turno testuale la smentisce",
        },
    },
    ColonnaCapability {
        nome: "thinking",
        lettura: Lettura::DallaTabella {
            punto: "mcp-core/src/orchestrator/core.rs:1965 (agentic_thinking_policy accanto)",
        },
        proprieta: Proprieta::DelFornitore,
        accertamento: Accertamento::SoloInviandoIlParametro {
            motivo: "si prova mandando la direttiva di thinking e guardando il rifiuto",
        },
    },
    // --- meccaniche di chiamata (nexus_provider_capabilities) ---------------
    ColonnaCapability {
        nome: "max_context_tokens",
        // MISURATO: nessun SELECT. Il valore che il gateway usa e' un altro,
        // costante per adapter (nexus-gateway/src/providers/*.rs), e per
        // kimi-k2.6 i due divergono di 4x senza che nulla lo dichiari.
        lettura: Lettura::NessunLettore,
        proprieta: Proprieta::DelFornitore,
        accertamento: Accertamento::DallEsercizio {
            segnale: "HTTP 400 context_length_exceeded (classify_provider_error)",
        },
    },
    ColonnaCapability {
        nome: "default_max_output_tokens",
        lettura: Lettura::NessunLettore,
        proprieta: Proprieta::NostraPolicy,
        accertamento: POLICY_NOSTRA,
    },
    ColonnaCapability {
        nome: "max_output_tokens_hard",
        lettura: Lettura::NessunLettore,
        proprieta: Proprieta::DelFornitore,
        accertamento: Accertamento::DallEsercizio {
            segnale: "HTTP 400 su max_tokens fuori range",
        },
    },
    ColonnaCapability {
        nome: "tool_choice_style",
        lettura: Lettura::ViaVista {
            punto: "mcp-core/src/capability.rs:88",
        },
        proprieta: Proprieta::DelFornitore,
        // La classe che il crate documenta per esteso: la prova esiste in
        // esercizio ma oggi non e' interpretabile, perche' l'osservazione non
        // registra se il parametro sia stato mandato.
        accertamento: Accertamento::SoloInviandoIlParametro {
            motivo: "un verdetto espresso senza forcing non prova che il forcing sia accettato",
        },
    },
    ColonnaCapability {
        nome: "tool_choice_first_turn_force",
        lettura: Lettura::NessunLettore,
        proprieta: Proprieta::NostraPolicy,
        accertamento: POLICY_NOSTRA,
    },
    ColonnaCapability {
        nome: "schema_strict",
        lettura: Lettura::NessunLettore,
        proprieta: Proprieta::DelFornitore,
        accertamento: DIALETTO_DA_INVIARE,
    },
    ColonnaCapability {
        nome: "schema_dialect",
        lettura: Lettura::NessunLettore,
        proprieta: Proprieta::DelFornitore,
        accertamento: DIALETTO_DA_INVIARE,
    },
    ColonnaCapability {
        nome: "tool_call_format",
        lettura: Lettura::NessunLettore,
        proprieta: Proprieta::DelFornitore,
        accertamento: DIALETTO_DA_INVIARE,
    },
    ColonnaCapability {
        nome: "max_tools_in_request",
        lettura: Lettura::NessunLettore,
        proprieta: Proprieta::DelFornitore,
        accertamento: Accertamento::DallEsercizio {
            segnale: "HTTP 400 su numero di tool eccessivo",
        },
    },
    ColonnaCapability {
        nome: "supports_prompt_cache",
        // FOSSILE, e lo dice gia' il codice: nexus-gateway/src/providers/
        // generic.rs:35-40. MISURATO il 10/08/2026: e' `false` per nove coppie
        // che nel ledger hanno letture di cache (mistral-small-latest: 2.461.120
        // token su 152 chiamate). Un valore FALSO e innocuo solo perche' morto.
        lettura: Lettura::NessunLettore,
        proprieta: Proprieta::DelFornitore,
        accertamento: Accertamento::DallEsercizio {
            segnale: "ai_usage_ledger.cache_read_tokens > 0",
        },
    },
    ColonnaCapability {
        nome: "prompt_cache_dialect",
        lettura: Lettura::NessunLettore,
        proprieta: Proprieta::DelFornitore,
        accertamento: DIALETTO_DA_INVIARE,
    },
    ColonnaCapability {
        nome: "supports_parallel_tools",
        lettura: Lettura::NessunLettore,
        proprieta: Proprieta::DelFornitore,
        accertamento: Accertamento::DallEsercizio {
            segnale: "piu' di una tool_call nello stesso turno",
        },
    },
    ColonnaCapability {
        nome: "stop_reason_dialect",
        lettura: Lettura::NessunLettore,
        proprieta: Proprieta::DelFornitore,
        accertamento: Accertamento::DallEsercizio {
            segnale: "il valore di stop_reason che le risposte portano davvero",
        },
    },
    ColonnaCapability {
        nome: "soft_failure_iter_threshold",
        lettura: Lettura::NessunLettore,
        proprieta: Proprieta::NostraPolicy,
        accertamento: POLICY_NOSTRA,
    },
    ColonnaCapability {
        nome: "soft_failure_content_threshold",
        lettura: Lettura::NessunLettore,
        proprieta: Proprieta::NostraPolicy,
        accertamento: POLICY_NOSTRA,
    },
    ColonnaCapability {
        nome: "history_keep_recent_messages",
        lettura: Lettura::NessunLettore,
        proprieta: Proprieta::NostraPolicy,
        accertamento: POLICY_NOSTRA,
    },
    ColonnaCapability {
        nome: "history_max_old_tool_result_chars",
        lettura: Lettura::NessunLettore,
        proprieta: Proprieta::NostraPolicy,
        accertamento: POLICY_NOSTRA,
    },
    ColonnaCapability {
        nome: "request_timeout_seconds",
        lettura: Lettura::NessunLettore,
        proprieta: Proprieta::NostraPolicy,
        accertamento: POLICY_NOSTRA,
    },
    ColonnaCapability {
        nome: "connect_timeout_seconds",
        lettura: Lettura::NessunLettore,
        proprieta: Proprieta::NostraPolicy,
        accertamento: POLICY_NOSTRA,
    },
    ColonnaCapability {
        nome: "tool_result_max_chars",
        lettura: Lettura::ViaVista {
            punto: "mcp-core/src/native_engine.rs:2413",
        },
        proprieta: Proprieta::NostraPolicy,
        accertamento: POLICY_NOSTRA,
    },
    ColonnaCapability {
        nome: "tool_result_max_bytes",
        lettura: Lettura::NessunLettore,
        proprieta: Proprieta::NostraPolicy,
        accertamento: POLICY_NOSTRA,
    },
    ColonnaCapability {
        nome: "tool_result_max_lines",
        lettura: Lettura::NessunLettore,
        proprieta: Proprieta::NostraPolicy,
        accertamento: POLICY_NOSTRA,
    },
    // --- flag semantici, seconda tornata (mig 0319 / 0478) ------------------
    ColonnaCapability {
        nome: "agentic_thinking_policy",
        lettura: Lettura::ViaVista {
            punto: "mcp-core/src/capability.rs:175",
        },
        proprieta: Proprieta::DelFornitore,
        accertamento: Accertamento::SoloInviandoIlParametro {
            motivo: "si prova mandando thinkingBudget=0 e guardando se il modello rifiuta",
        },
    },
    ColonnaCapability {
        nome: "image_gen",
        lettura: PUNTO_MEDIA,
        proprieta: Proprieta::DelFornitore,
        accertamento: Accertamento::SoloInviandoIlParametro {
            motivo: "si prova chiedendo un'immagine",
        },
    },
    ColonnaCapability {
        nome: "audio_in",
        lettura: PUNTO_MEDIA,
        proprieta: Proprieta::DelFornitore,
        accertamento: Accertamento::SoloInviandoIlParametro {
            motivo: "si prova mandando audio",
        },
    },
    ColonnaCapability {
        nome: "audio_out",
        lettura: PUNTO_MEDIA,
        proprieta: Proprieta::DelFornitore,
        accertamento: Accertamento::SoloInviandoIlParametro {
            motivo: "si prova chiedendo audio",
        },
    },
    ColonnaCapability {
        nome: "video_gen",
        lettura: PUNTO_MEDIA,
        proprieta: Proprieta::DelFornitore,
        accertamento: Accertamento::SoloInviandoIlParametro {
            motivo: "si prova chiedendo un video",
        },
    },
    // --- tetto di output dichiarato dal wire (mig 0716) ----------------------
    ColonnaCapability {
        nome: "declared_max_output_tokens",
        // Il consumatore con CONSEGUENZE legge dalla TABELLA
        // (`fetch_dichiarazione_wire`), sul ramo in cui la vista tace per
        // costruzione (LEFT JOIN guidata dalla capability: il modello da
        // discovery non vi compare). La colonna in vista serve alla coppia
        // curata per l'audit dei mismatch, e li' viene letta e ignorata
        // (`fetch_fatti_tetto`, quarta colonna). Il tipo ammette UN punto:
        // si dichiara quello che decide.
        lettura: Lettura::DallaTabella {
            punto: "mcp-core/src/capability.rs (fetch_dichiarazione_wire)",
        },
        proprieta: Proprieta::DelFornitore,
        accertamento: Accertamento::DallEsercizio {
            segnale: "finish_reason=length / HTTP 400-402 su un tetto dichiarato piu' alto del vero",
        },
    },
];

/// Le colonne che nessuno legge, in ordine di vista. E' il numero che il
/// censimento riporta per primo: dice quanta parte della «fonte unica della
/// capability» non abbia consumatori.
pub fn senza_lettore() -> impl Iterator<Item = &'static ColonnaCapability> {
    COLONNE.iter().filter(|c| !c.lettura.ha_un_lettore())
}

/// La colonna dal nome, se dichiarata. `None` significa «colonna non nel
/// vocabolario», che il guard rende impossibile per le colonne della vista ma
/// resta possibile per un nome inventato dal chiamante.
pub fn colonna(nome: &str) -> Option<&'static ColonnaCapability> {
    COLONNE.iter().find(|c| c.nome == nome)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn nessun_nome_ripetuto() {
        // Un duplicato renderebbe `colonna()` dipendente dall'ordine e il
        // confronto col guard verde per la ragione sbagliata.
        let mut nomi: Vec<&str> = COLONNE.iter().map(|c| c.nome).collect();
        let totali = nomi.len();
        nomi.sort_unstable();
        nomi.dedup();
        assert_eq!(nomi.len(), totali, "nomi di colonna duplicati nel vocabolario");
    }

    #[test]
    fn la_policy_nostra_non_e_mai_accertabile_dallesercizio() {
        // Il vincolo che tiene insieme due dei tre campi: se una colonna e'
        // NOSTRA politica, non esiste un esperimento che ne dica il valore
        // «vero». Dichiararla accertabile dall'esercizio significherebbe
        // progettare un probe per una domanda che non ha risposta.
        for c in COLONNE.iter().filter(|c| c.proprieta == Proprieta::NostraPolicy) {
            assert!(
                matches!(c.accertamento, Accertamento::NonAccertabile { .. }),
                "{}: e' una nostra politica, non puo' essere accertata dal fornitore",
                c.nome
            );
        }
    }

    #[test]
    fn identita_non_e_capability() {
        for c in COLONNE.iter().filter(|c| c.proprieta == Proprieta::Identita) {
            assert!(matches!(c.accertamento, Accertamento::NonAccertabile { .. }));
        }
        assert_eq!(
            COLONNE
                .iter()
                .filter(|c| c.proprieta == Proprieta::Identita)
                .count(),
            2,
            "l'identita' della riga e' (provider, model)"
        );
    }

    #[test]
    fn la_misura_del_10_08_2026_e_dichiarata() {
        // Il numero che giustifica il modulo, tenuto come test perche' e' cio'
        // che cambierebbe se qualcuno collegasse (o scollegasse) una colonna:
        // 3 di dati lette dalla vista (5 con l'identita'), 8 dalla tabella
        // (le 7 misurate il 10/08 piu' `declared_max_output_tokens`, mig 0716),
        // 20 senza alcun lettore.
        let via_vista = COLONNE
            .iter()
            .filter(|c| matches!(c.lettura, Lettura::ViaVista { .. }))
            .count();
        let dalla_tabella = COLONNE
            .iter()
            .filter(|c| matches!(c.lettura, Lettura::DallaTabella { .. }))
            .count();
        let orfane = senza_lettore().count();
        // provider e model contano fra le lette-via-vista: sono la clausola
        // WHERE delle tre query, non tre colonne di dati in piu'.
        assert_eq!((via_vista, dalla_tabella, orfane), (5, 8, 20));
        assert_eq!(via_vista + dalla_tabella + orfane, COLONNE.len());
    }

    #[test]
    fn le_orfane_restano_nominate() {
        // Un elenco che si accorcia in silenzio nasconderebbe proprio il fatto
        // che il modulo esiste per rendere visibile. Due nomi verificati a mano
        // il 10/08/2026 devono restare fra le orfane finche' nessuno le collega.
        let nomi: Vec<&str> = senza_lettore().map(|c| c.nome).collect();
        assert!(nomi.contains(&"supports_prompt_cache"));
        assert!(nomi.contains(&"max_context_tokens"));
    }
}
