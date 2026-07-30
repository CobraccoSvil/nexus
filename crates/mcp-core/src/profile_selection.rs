//! Selezione automatica del profilo (`profile_id == "auto"`): punto unico
//! (regola L) del richiamo SEMANTICO fra la richiesta e i profili candidati.
//!
//! PRIMA (`profiles::auto_select_profile`) il punteggio era un conteggio di
//! token (>3 caratteri) del profilo trovati come sottostringa nella richiesta,
//! su uno slice A 300 BYTE del system_prompt — su una stringa UTF-8 uno slice a
//! byte fisso puo' cadere in mezzo a un carattere multi-byte: bastava un
//! profilo con un accento nella posizione giusta per far PANICARE l'handler
//! dell'intera chat. Il punteggio premiava meccanicamente il prompt piu' lungo
//! (piu' token da matchare, piu' probabilita' di un hit per caso) e non aveva
//! un'uscita "nessun profilo pertinente": vinceva sempre il candidato migliore,
//! fosse anche a un solo token di distanza.
//!
//! Il fix usa la similarita' coseno fra l'embedding della richiesta e
//! l'embedding del testo del profilo, calcolati dallo STESSO embedder
//! in-process gia' usato da `prompt_memories` (niente round-trip a un servizio
//! esterno, niente Qdrant: i candidati sono una manciata di profili in memoria,
//! non un indice da interrogare). Sotto la soglia configurabile nessun profilo
//! e' pertinente: si ritorna il default esplicito, non il meno-peggio.
//!
//! Il confine esterno (imbedding) e' [`nexus_agent_tools::context_core::TextEmbedder`],
//! lo stesso trait implementato da `NeuralCoreClient` per `agent_tools::context`:
//! un test misura il criterio (soglia, confronto, uscita esplicita) con un
//! embedder finto a vettori noti, senza dipendere dal modello ONNX vero
//! (regola O).

use nexus_agent_tools::context_core::TextEmbedder;

/// Sotto questa similarita' coseno un profilo non e' considerato pertinente
/// alla richiesta. Sorgente: `settings.orchestrator.profile_auto_select_min_similarity`
/// (mig 0658). Il vecchio criterio (conteggio sottostringhe) non aveva soglia:
/// vinceva sempre il migliore fra i candidati, anche a un solo token di distanza.
pub(crate) const DEFAULT_MIN_SIMILARITY: f32 = 0.55;

/// Quanti caratteri (non byte: regola del difetto collaterale) del testo di un
/// profilo entrano nell'embedding. Limita il costo dell'embedder su
/// system_prompt lunghi senza tagliare a meta' un carattere multi-byte.
const PROFILE_TEXT_MAX_CHARS: usize = 600;

/// Un profilo candidato: l'indice originale (per recuperarlo dopo) + il testo
/// su cui si calcola la similarita' (nome + descrizione + inizio del
/// system_prompt, gia' troncato per caratteri).
pub(crate) struct ProfileCandidate {
    pub idx: usize,
    pub text: String,
}

impl ProfileCandidate {
    /// Costruisce il testo del candidato troncando per CARATTERI, mai per byte
    /// (il difetto che questo modulo chiude: vedi doc del modulo).
    pub(crate) fn new(idx: usize, name: &str, description: &str, system_prompt: &str) -> Self {
        let prompt_head: String = system_prompt.chars().take(PROFILE_TEXT_MAX_CHARS).collect();
        Self {
            idx,
            text: format!("{name} {description} {prompt_head}"),
        }
    }
}

/// Sceglie l'indice del profilo piu' vicino semanticamente a `query`, sopra
/// `min_similarity`. `None` = nessun profilo pertinente (candidati vuoti,
/// embedding della domanda fallito, o il migliore comunque sotto soglia) — il
/// chiamante ritorna il default esplicito, mai un candidato indovinato.
pub(crate) async fn select_best_profile(
    embedder: &dyn TextEmbedder,
    query: &str,
    candidates: &[ProfileCandidate],
    min_similarity: f32,
) -> Option<usize> {
    if candidates.is_empty() {
        return None;
    }
    let query_vec = match embedder.embed_text("", query).await {
        Ok(v) => v,
        Err(e) => {
            tracing::warn!("profile_selection: embedding della richiesta fallito ({e})");
            return None;
        }
    };
    let mut best: Option<(usize, f32)> = None;
    for c in candidates {
        let Ok(v) = embedder.embed_text("", &c.text).await else {
            // Un profilo il cui embedding fallisce non partecipa al confronto:
            // non e' un candidato scartato per pertinenza, e' un candidato che
            // non si e' potuto giudicare.
            continue;
        };
        let sim = cosine_similarity(&query_vec, &v);
        if best.is_none_or(|(_, best_sim)| sim > best_sim) {
            best = Some((c.idx, sim));
        }
    }
    best.filter(|(_, sim)| *sim >= min_similarity)
        .map(|(idx, _)| idx)
}

/// Similarita' coseno fra due vettori. `0.0` se le dimensioni non combaciano
/// (embedder cambiato a runtime) o uno dei due e' nullo: un valore che non puo'
/// mai superare una soglia positiva, quindi degrada a "non pertinente", mai a un
/// match spurio.
fn cosine_similarity(a: &[f32], b: &[f32]) -> f32 {
    if a.is_empty() || a.len() != b.len() {
        return 0.0;
    }
    let dot: f32 = a.iter().zip(b).map(|(x, y)| x * y).sum();
    let norm_a: f32 = a.iter().map(|x| x * x).sum::<f32>().sqrt();
    let norm_b: f32 = b.iter().map(|x| x * x).sum::<f32>().sqrt();
    if norm_a == 0.0 || norm_b == 0.0 {
        return 0.0;
    }
    dot / (norm_a * norm_b)
}

#[cfg(test)]
mod tests {
    use super::*;
    use futures::future::BoxFuture;

    /// Embedder finto a vettori NOTI: il test misura il criterio (soglia,
    /// confronto, uscita esplicita), non il modello ONNX vero (regola O). Le
    /// parole del testo sono mappate a coordinate fisse cosi' la similarita' e'
    /// prevedibile: testi che condividono parole hanno prodotto vicino,
    /// altrimenti ortogonale.
    #[derive(Debug)]
    struct FakeEmbedder;

    impl TextEmbedder for FakeEmbedder {
        fn embed_text<'a>(
            &'a self,
            _model: &'a str,
            text: &'a str,
        ) -> BoxFuture<'a, Result<Vec<f32>, String>> {
            Box::pin(async move {
                let mut v = vec![0.0f32; 4];
                for word in text.to_lowercase().split_whitespace() {
                    match word {
                        "rust" | "backend" | "cargo" => v[0] += 1.0,
                        "react" | "frontend" | "typescript" => v[1] += 1.0,
                        "docker" | "deploy" | "kubernetes" => v[2] += 1.0,
                        _ => v[3] += 0.1,
                    }
                }
                Ok(v)
            })
        }
    }

    /// Embedder che fallisce sempre: misura che un guasto dell'embedder
    /// degradi a "nessun profilo pertinente", mai a un panico o a un match
    /// indovinato.
    #[derive(Debug)]
    struct FailingEmbedder;

    impl TextEmbedder for FailingEmbedder {
        fn embed_text<'a>(
            &'a self,
            _model: &'a str,
            _text: &'a str,
        ) -> BoxFuture<'a, Result<Vec<f32>, String>> {
            Box::pin(async move { Err("embedder down".to_string()) })
        }
    }

    fn candidates() -> Vec<ProfileCandidate> {
        vec![
            ProfileCandidate::new(0, "Rust backend", "sviluppo Rust e Cargo", ""),
            ProfileCandidate::new(1, "React frontend", "sviluppo React e TypeScript", ""),
            ProfileCandidate::new(2, "DevOps", "Docker e Kubernetes deploy", ""),
        ]
    }

    #[tokio::test]
    async fn sceglie_il_candidato_semanticamente_vicino() {
        let idx = select_best_profile(
            &FakeEmbedder,
            "aiutami con un bug nel backend Rust",
            &candidates(),
            DEFAULT_MIN_SIMILARITY,
        )
        .await;
        assert_eq!(idx, Some(0));
    }

    #[tokio::test]
    async fn sceglie_un_candidato_diverso_per_dominio_diverso() {
        let idx = select_best_profile(
            &FakeEmbedder,
            "come struttura i componenti React in TypeScript?",
            &candidates(),
            DEFAULT_MIN_SIMILARITY,
        )
        .await;
        assert_eq!(idx, Some(1));
    }

    /// Il caso che chiude la trappola del vecchio criterio: un testo SENZA
    /// nessuna parola dei profili non deve vincere per il profilo piu' lungo,
    /// deve dichiarare "nessuno pertinente".
    #[tokio::test]
    async fn nessun_profilo_pertinente_sotto_soglia() {
        let idx = select_best_profile(
            &FakeEmbedder,
            "che tempo fa oggi",
            &candidates(),
            DEFAULT_MIN_SIMILARITY,
        )
        .await;
        assert_eq!(idx, None);
    }

    #[tokio::test]
    async fn candidati_vuoti_ritorna_none() {
        let idx =
            select_best_profile(&FakeEmbedder, "qualunque cosa", &[], DEFAULT_MIN_SIMILARITY)
                .await;
        assert_eq!(idx, None);
    }

    #[tokio::test]
    async fn embedder_fallito_degrada_a_nessun_profilo() {
        let idx = select_best_profile(
            &FailingEmbedder,
            "aiutami con Rust",
            &candidates(),
            DEFAULT_MIN_SIMILARITY,
        )
        .await;
        assert_eq!(idx, None);
    }

    #[test]
    fn cosine_similarity_identici_e_ortogonali() {
        assert!((cosine_similarity(&[1.0, 0.0], &[1.0, 0.0]) - 1.0).abs() < 1e-6);
        assert!(cosine_similarity(&[1.0, 0.0], &[0.0, 1.0]).abs() < 1e-6);
        assert_eq!(cosine_similarity(&[], &[]), 0.0);
        assert_eq!(cosine_similarity(&[1.0], &[1.0, 2.0]), 0.0);
    }

    /// Il difetto collaterale che questo modulo chiude: un profilo con
    /// caratteri multi-byte (accenti) nel system_prompt oltre la soglia di
    /// troncamento NON deve panicare — prima uno slice a 300 BYTE poteva
    /// cadere in mezzo a un carattere.
    #[test]
    fn tronca_per_caratteri_non_panica_su_multibyte() {
        let accentato = "à".repeat(PROFILE_TEXT_MAX_CHARS + 50);
        let c = ProfileCandidate::new(0, "n", "d", &accentato);
        // Se fosse uno slice a byte fisso su UTF-8, questo avrebbe gia'
        // panicato in `new`: arrivare qui e' l'asserzione.
        assert!(c.text.chars().count() <= PROFILE_TEXT_MAX_CHARS + "n d ".len());
    }
}
