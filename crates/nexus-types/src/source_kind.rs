//! `source_kind`: PUNTO UNICO (regola L) del vocabolario delle sorgenti
//! indicizzabili dal RAG — quali tipologie esistono, come si scrivono sul wire,
//! e quali filtri di payload ammettono.
//!
//! # Perche' vive qui e non accanto al RAG
//!
//! Nasceva in `mcp-core::rag`, accanto a chi interroga Qdrant. Ma lo stesso
//! vocabolario e' anche quello che il tool `nexus_search_semantic` PROMETTE al
//! modello, e lo schema di quel tool sta in `nexus-agent-tools` — un crate che
//! `mcp-core` non puo' esportargli, perche' la dipendenza va nell'altro verso.
//!
//! Finche' lo schema era JSON scritto a mano la duplicazione non si vedeva, e
//! infatti aveva gia' prodotto una divergenza: il catalogo elencava CINQUE
//! valori mentre `parse` ne accettava OTTO, e i tre in piu' — le collection
//! legacy che un commento dichiarava «esposte tramite il canale unico
//! nexus_search_semantic» — erano raggiungibili dal codice ma non chiedibili da
//! chi doveva chiederle. Col vocabolario qui, lo schema del tool si DERIVA da
//! [`SourceKind::VALORI`] e quella divergenza non e' piu' rappresentabile.
//!
//! `mcp-core::rag` resta il re-export: i call site storici non cambiano.

use serde::{Deserialize, Serialize};

/// Tipologie di sorgenti indicizzabili.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SourceKind {
    Attachment,
    Kb,
    ChatHistory,
    ToolResult,
    Code,
    // Collection legacy esposte tramite il canale unico nexus_search_semantic.
    // Payload eterogeneo gestito in search.rs.
    MetaDoc,
    Conversation,
    PromptCorrection,
}

impl SourceKind {
    /// Il vocabolario completo, nell'ordine in cui e' dichiarato.
    ///
    /// Esiste perche' lo schema del tool lo DERIVI invece di ripeterlo: era
    /// proprio quella ripetizione a essersi disallineata. Una sorgente nuova
    /// entra qui e compare automaticamente fra i valori che il modello puo'
    /// chiedere.
    pub const TUTTI: [SourceKind; 8] = [
        Self::Attachment,
        Self::Kb,
        Self::ChatHistory,
        Self::ToolResult,
        Self::Code,
        Self::MetaDoc,
        Self::Conversation,
        Self::PromptCorrection,
    ];

    /// Gli stessi valori come stringhe di wire, per chi espone il vocabolario
    /// come elenco (lo schema del tool).
    pub const VALORI: [&'static str; 8] = [
        Self::Attachment.as_str(),
        Self::Kb.as_str(),
        Self::ChatHistory.as_str(),
        Self::ToolResult.as_str(),
        Self::Code.as_str(),
        Self::MetaDoc.as_str(),
        Self::Conversation.as_str(),
        Self::PromptCorrection.as_str(),
    ];

    /// Il valore canonico sul wire (regola N): quello che il modello scrive nel
    /// campo `source_kinds` e quello che finisce nel payload Qdrant.
    pub const fn as_str(&self) -> &'static str {
        match self {
            SourceKind::Attachment => "attachment",
            SourceKind::Kb => "kb",
            SourceKind::ChatHistory => "chat_history",
            SourceKind::ToolResult => "tool_result",
            SourceKind::Code => "code",
            SourceKind::MetaDoc => "meta_doc",
            SourceKind::Conversation => "conversation",
            SourceKind::PromptCorrection => "prompt_correction",
        }
    }

    /// Il kind da un valore di wire, `None` se non appartiene al vocabolario.
    ///
    /// Deriva da [`Self::TUTTI`] invece di ripetere un `match`: era proprio la
    /// seconda copia dell'elenco a poter divergere, ed e' divergente per mesi
    /// dal terzo — l'`enum` che il catalogo prometteva al modello.
    pub fn parse(s: &str) -> Option<Self> {
        Self::TUTTI.into_iter().find(|k| k.as_str() == s)
    }

    /// True se la collection del kind ha `project_id` nel payload (filtrabile).
    /// Conversation usa session_id; MetaDoc e' globale (nessun filtro project).
    pub fn supports_project_filter(&self) -> bool {
        !matches!(self, SourceKind::Conversation | SourceKind::MetaDoc)
    }

    /// True se il kind filtra per session_id (chat conversazionali).
    pub fn uses_session_filter(&self) -> bool {
        matches!(self, SourceKind::ChatHistory | SourceKind::Conversation)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Le tre forme del vocabolario — le varianti, i valori di wire e il parse —
    /// restano una cosa sola.
    ///
    /// MUTAZIONE: aggiungendo una variante senza metterla in `TUTTI`, il
    /// conteggio non torna piu' e questo test rosseggia prima che lo schema del
    /// tool prometta meno di quanto `parse` accetti — che e' esattamente il
    /// difetto misurato (catalogo con 5 valori, `parse` con 8).
    #[test]
    fn il_vocabolario_ha_una_forma_sola() {
        assert_eq!(SourceKind::TUTTI.len(), SourceKind::VALORI.len());
        for (kind, valore) in SourceKind::TUTTI.iter().zip(SourceKind::VALORI) {
            assert_eq!(kind.as_str(), valore);
            assert_eq!(
                SourceKind::parse(valore),
                Some(*kind),
                "il parse riconosce cio' che il vocabolario dichiara"
            );
            assert_eq!(
                serde_json::to_value(kind).expect("serializza"),
                serde_json::json!(valore),
                "serde scrive lo stesso valore di as_str"
            );
        }
        assert_eq!(SourceKind::parse("inventato"), None);
    }
}
