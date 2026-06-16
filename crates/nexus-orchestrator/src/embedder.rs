//! Text embedder per converting task descriptions in vettori
//!
//! Due implementazioni:
//! - `HashEmbedder`: bag-of-words deterministico, nessuna dipendenza, usato come fallback.
//! - `OnnxMiniLmEmbedder`: `all-MiniLM-L6-v2` in ONNX (384-d), caricato a runtime se il
//!   file modello è presente. Usa ONNX Runtime via `ort` + tokenizer HuggingFace via
//!   `tokenizers`.  Fallback automatico a `HashEmbedder` se il file non esiste.
//!
//! Percorsi di default (sovrascrivibili via env):
//! - `NEXUS_MINILM_MODEL`      → `models/minilm/model.onnx`
//! - `NEXUS_MINILM_TOKENIZER`  → `models/minilm/tokenizer.json`

use std::collections::HashMap;
#[cfg(feature = "onnx")]
use std::sync::OnceLock;

/// Embedder semplice per testi
pub trait Embedder: Send + Sync {
    /// Dimensione dei vettori prodotti
    fn dim(&self) -> usize;

    /// Converte testo in vettore
    fn embed(&self, text: &str) -> Vec<f32>;

    /// Batch embedding
    fn embed_batch(&self, texts: &[&str]) -> Vec<Vec<f32>> {
        texts.iter().map(|t| self.embed(t)).collect()
    }

    /// Signature stabile dell'embedder: identifica univocamente lo spazio
    /// vettoriale prodotto. Formato `"<nome>:<dim>"` (es. `"onnx-minilm-l6-v2:384"`
    /// o `"hash:256"`).
    ///
    /// Usata per invalidare gli hash di indicizzazione quando l'embedder cambia:
    /// includere la signature nel digest fa sì che il reindex automatico riparta
    /// da solo al cambio modello, senza azzerare gli hash a mano.
    fn signature(&self) -> String {
        format!("{}:{}", self.name(), self.dim())
    }

    /// Nome stabile dell'embedder concreto (senza dimensione).
    fn name(&self) -> &'static str;
}

/// Embedder semplice basato su hashing di n-grams
/// Genera vettori deterministici, utile per testing
pub struct HashEmbedder {
    dim: usize,
}

impl HashEmbedder {
    pub fn new(dim: usize) -> Self {
        Self { dim }
    }

    /// Hash una stringa a un indice [0, dim)
    fn hash_to_idx(&self, s: &str, seed: u64) -> usize {
        let mut hash: u64 = seed;
        for b in s.bytes() {
            hash = hash.wrapping_mul(31).wrapping_add(b as u64);
        }
        (hash % self.dim as u64) as usize
    }

    /// Tokenizzazione semplice (lowercase + split su whitespace/punct)
    fn tokenize(text: &str) -> Vec<String> {
        text.to_lowercase()
            .split(|c: char| !c.is_alphanumeric())
            .filter(|s| !s.is_empty() && s.len() > 1)
            .map(|s| s.to_string())
            .collect()
    }

    /// Estrae n-grams (default: unigrams + bigrams)
    fn extract_ngrams(tokens: &[String]) -> Vec<String> {
        let mut ngrams = Vec::new();
        // Unigrams
        for t in tokens {
            ngrams.push(t.clone());
        }
        // Bigrams
        for window in tokens.windows(2) {
            ngrams.push(format!("{}_{}", window[0], window[1]));
        }
        ngrams
    }
}

impl Embedder for HashEmbedder {
    fn dim(&self) -> usize {
        self.dim
    }

    fn name(&self) -> &'static str {
        "hash"
    }

    fn embed(&self, text: &str) -> Vec<f32> {
        let mut vec = vec![0.0_f32; self.dim];

        let tokens = Self::tokenize(text);
        let ngrams = Self::extract_ngrams(&tokens);

        if ngrams.is_empty() {
            return vec;
        }

        // Accumula pesi sugli indici hashati
        for ngram in &ngrams {
            let idx = self.hash_to_idx(ngram, 0);
            vec[idx] += 1.0;

            // Seconda hash function per ridurre collisioni (feature hashing)
            let idx2 = self.hash_to_idx(ngram, 17);
            let sign = if (idx2 & 1) == 0 { 1.0 } else { -1.0 };
            vec[idx2 % self.dim] += sign * 0.5;
        }

        // Normalizzazione L2
        let norm: f32 = vec.iter().map(|x| x * x).sum::<f32>().sqrt();
        if norm > 0.0 {
            for v in vec.iter_mut() {
                *v /= norm;
            }
        }

        vec
    }
}

/// Cache per embedding (evita re-embedding dello stesso testo)
pub struct CachedEmbedder<E: Embedder> {
    inner: E,
    cache: parking_lot::RwLock<HashMap<String, Vec<f32>>>,
    max_cache_size: usize,
}

impl<E: Embedder> CachedEmbedder<E> {
    pub fn new(inner: E, max_cache_size: usize) -> Self {
        Self {
            inner,
            cache: parking_lot::RwLock::new(HashMap::new()),
            max_cache_size,
        }
    }

    pub fn clear_cache(&self) {
        self.cache.write().clear();
    }

    pub fn cache_size(&self) -> usize {
        self.cache.read().len()
    }
}

impl<E: Embedder> Embedder for CachedEmbedder<E> {
    fn dim(&self) -> usize {
        self.inner.dim()
    }

    fn name(&self) -> &'static str {
        self.inner.name()
    }

    fn embed(&self, text: &str) -> Vec<f32> {
        // Check cache
        if let Some(cached) = self.cache.read().get(text) {
            return cached.clone();
        }

        // Miss: compute and store
        let vec = self.inner.embed(text);

        let mut cache = self.cache.write();
        if cache.len() >= self.max_cache_size {
            // Semplice eviction: clear tutto quando pieno
            // In production: LRU eviction
            cache.clear();
        }
        cache.insert(text.to_string(), vec.clone());

        vec
    }
}

// ---------------------------------------------------------------------------
// OnnxMiniLmEmbedder — `all-MiniLM-L6-v2` semantico a 384 dimensioni
// ---------------------------------------------------------------------------

/// Dimensione output di all-MiniLM-L6-v2.
pub const MINILM_DIM: usize = 384;

/// Percorso modello ONNX di default (relativo alla cwd del processo).
pub const DEFAULT_MODEL_PATH: &str = "models/minilm/model.onnx";
/// Percorso tokenizer di default.
pub const DEFAULT_TOKENIZER_PATH: &str = "models/minilm/tokenizer.json";

// Imports ONNX usati solo quando il feature `onnx` è abilitato
#[cfg(feature = "onnx")]
use ort::session::Session;
#[cfg(feature = "onnx")]
use ort::session::builder::GraphOptimizationLevel;
#[cfg(feature = "onnx")]
use ort::value::Tensor;

// ---------------------------------------------------------------------------
// OnnxMiniLmEmbedder — implementazione con feature `onnx`
// ---------------------------------------------------------------------------

/// Embedder basato su `all-MiniLM-L6-v2` in formato ONNX.
///
/// Disponibile solo con feature `onnx` (richiede AVX2).
/// Senza il feature, `try_from_env()` ritorna sempre `Err` e `NexusBridge`
/// fa fallback automatico a `HashEmbedder(256)`.
///
/// Thread-safety: la `Session` ORT è avvolta in `parking_lot::Mutex` per
/// soddisfare `Send + Sync` richiesto dal trait `Embedder`.
#[cfg(feature = "onnx")]
pub struct OnnxMiniLmEmbedder {
    session: parking_lot::Mutex<Session>,
    tokenizer: tokenizers::Tokenizer,
}

/// Stub quando il feature `onnx` non è abilitato.
/// `try_from_env()` ritorna sempre Err; il chiamante usa HashEmbedder.
#[cfg(not(feature = "onnx"))]
pub struct OnnxMiniLmEmbedder {
    _private: (),
}

/// Implementazione completa con ONNX Runtime (richiede AVX2)
#[cfg(feature = "onnx")]
impl OnnxMiniLmEmbedder {
    /// Carica modello e tokenizer dai percorsi forniti.
    pub fn try_from_paths(
        model_path: &str,
        tokenizer_path: &str,
    ) -> anyhow::Result<Self> {
        // Verifica esistenza file prima di provare a caricare ORT
        if !std::path::Path::new(model_path).exists() {
            anyhow::bail!("model file not found: {}", model_path);
        }
        if !std::path::Path::new(tokenizer_path).exists() {
            anyhow::bail!("tokenizer file not found: {}", tokenizer_path);
        }

        let session = Session::builder()
            .map_err(|e| anyhow::anyhow!("ORT builder init error: {e}"))?
            .with_optimization_level(GraphOptimizationLevel::All)
            .map_err(|e| anyhow::anyhow!("ORT optimization level error: {e}"))?
            .commit_from_file(model_path)
            .map_err(|e| anyhow::anyhow!("ORT commit from file error: {e}"))?;

        let tokenizer = tokenizers::Tokenizer::from_file(tokenizer_path)
            .map_err(|e| anyhow::anyhow!("tokenizer load error: {e}"))?;

        Ok(Self {
            session: parking_lot::Mutex::new(session),
            tokenizer,
        })
    }

    /// Carica usando le env var `NEXUS_MINILM_MODEL` / `NEXUS_MINILM_TOKENIZER`
    /// con fallback ai percorsi di default.
    pub fn try_from_env() -> anyhow::Result<Self> {
        let model_path = std::env::var("NEXUS_MINILM_MODEL")
            .unwrap_or_else(|_| DEFAULT_MODEL_PATH.to_string());
        let tokenizer_path = std::env::var("NEXUS_MINILM_TOKENIZER")
            .unwrap_or_else(|_| DEFAULT_TOKENIZER_PATH.to_string());
        Self::try_from_paths(&model_path, &tokenizer_path)
    }

    /// Normalizzazione L2 in-place.
    fn l2_normalize(v: &mut [f32]) {
        let norm: f32 = v.iter().map(|x| x * x).sum::<f32>().sqrt();
        if norm > 1e-9 {
            for x in v.iter_mut() {
                *x /= norm;
            }
        }
    }
}

// SAFETY: Session è thread-safe sotto Mutex; Tokenizer è Send+Sync.
#[cfg(feature = "onnx")]
unsafe impl Send for OnnxMiniLmEmbedder {}
#[cfg(feature = "onnx")]
unsafe impl Sync for OnnxMiniLmEmbedder {}

#[cfg(feature = "onnx")]
impl Embedder for OnnxMiniLmEmbedder {
    fn dim(&self) -> usize {
        MINILM_DIM
    }

    fn name(&self) -> &'static str {
        "onnx-minilm-l6-v2"
    }

    fn embed(&self, text: &str) -> Vec<f32> {
        // ── Tokenizzazione ────────────────────────────────────────────────
        // add_special_tokens=true: aggiunge [CLS]/[SEP] come fa SentenceTransformer.
        // SENZA di essi il mean-pooling opera su token diversi e i vettori
        // divergono dal modello PyTorch (parita' cosine crollava a 0.33 sulle
        // frasi corte) -> i vettori gia' indicizzati in Qdrant sarebbero
        // incompatibili. Il tokenizer BERT applica il TemplateProcessing.
        let encoding = match self.tokenizer.encode(text, true) {
            Ok(e) => e,
            Err(e) => {
                tracing::warn!("MiniLM tokenize error: {e}");
                return vec![0.0; MINILM_DIM];
            }
        };

        let ids: Vec<i64> = encoding.get_ids().iter().map(|&x| x as i64).collect();
        let mask: Vec<i64> = encoding
            .get_attention_mask()
            .iter()
            .map(|&x| x as i64)
            .collect();
        let type_ids: Vec<i64> = encoding
            .get_type_ids()
            .iter()
            .map(|&x| x as i64)
            .collect();

        let seq_len = ids.len();
        if seq_len == 0 {
            return vec![0.0; MINILM_DIM];
        }

        // ── Build ORT tensor inputs — (shape, Vec<T>), no ndarray needed ──
        let ids_tensor = match Tensor::<i64>::from_array(([1_usize, seq_len], ids)) {
            Ok(t) => t,
            Err(e) => {
                tracing::warn!("MiniLM ids tensor error: {e}");
                return vec![0.0; MINILM_DIM];
            }
        };
        let mask_tensor = match Tensor::<i64>::from_array(([1_usize, seq_len], mask.clone())) {
            Ok(t) => t,
            Err(e) => {
                tracing::warn!("MiniLM mask tensor error: {e}");
                return vec![0.0; MINILM_DIM];
            }
        };
        let type_ids_tensor =
            match Tensor::<i64>::from_array(([1_usize, seq_len], type_ids)) {
                Ok(t) => t,
                Err(e) => {
                    tracing::warn!("MiniLM type_ids tensor error: {e}");
                    return vec![0.0; MINILM_DIM];
                }
            };

        // ── Inference ─────────────────────────────────────────────────────
        let mut session = self.session.lock();
        let outputs = match session.run(ort::inputs![
            "input_ids"      => ids_tensor,
            "attention_mask" => mask_tensor,
            "token_type_ids" => type_ids_tensor,
        ]) {
            Ok(o) => o,
            Err(e) => {
                tracing::warn!("MiniLM inference error: {e}");
                return vec![0.0; MINILM_DIM];
            }
        };

        // ── Estrazione last_hidden_state ───────────────────────────────────
        // try_extract_tensor returns (&Shape, &[f32]) — flat slice, no ndarray
        // Shape: [1, seq_len, MINILM_DIM]
        let (_shape, data) = match outputs["last_hidden_state"].try_extract_tensor::<f32>() {
            Ok(t) => t,
            Err(e) => {
                tracing::warn!("MiniLM output extract error: {e}");
                return vec![0.0; MINILM_DIM];
            }
        };

        // ── Mean pooling ──────────────────────────────────────────────────
        // data layout (flattened [1, seq_len, MINILM_DIM]):
        //   element [0, t, d] → data[t * MINILM_DIM + d]
        let mut pooled = vec![0.0_f32; MINILM_DIM];
        let mut mask_sum = 0.0_f32;

        for t in 0..seq_len {
            let m = mask.get(t).copied().unwrap_or(0) as f32;
            mask_sum += m;
            let base = t * MINILM_DIM;
            if base + MINILM_DIM <= data.len() {
                for d in 0..MINILM_DIM {
                    pooled[d] += data[base + d] * m;
                }
            }
        }
        if mask_sum > 0.0 {
            for v in pooled.iter_mut() {
                *v /= mask_sum;
            }
        }

        // ── L2 normalize ──────────────────────────────────────────────────
        Self::l2_normalize(&mut pooled);
        pooled
    }
}

// Lazy singleton ORT environment (ort 2.x lo gestisce internamente, ma utile per logging)
#[cfg(feature = "onnx")]
static _ORT_INIT: OnceLock<()> = OnceLock::new();

pub fn init_ort_logging() {
    #[cfg(feature = "onnx")]
    _ORT_INIT.get_or_init(|| {
        // ort 2.x inizializza automaticamente; questa fn è un no-op documentato
    });
}

// ---------------------------------------------------------------------------
// Stub quando feature `onnx` è disabilitato
// ---------------------------------------------------------------------------

/// Stub: impl OnnxMiniLmEmbedder senza ONNX Runtime.
/// Tutti i metodi ritornano Err o fallback, NexusBridge usa HashEmbedder.
#[cfg(not(feature = "onnx"))]
impl OnnxMiniLmEmbedder {
    pub fn try_from_paths(_model: &str, _tok: &str) -> anyhow::Result<Self> {
        anyhow::bail!("OnnxMiniLmEmbedder non disponibile: compilare con feature 'onnx'")
    }

    pub fn try_from_env() -> anyhow::Result<Self> {
        anyhow::bail!("OnnxMiniLmEmbedder non disponibile: compilare con feature 'onnx'")
    }
}

#[cfg(not(feature = "onnx"))]
unsafe impl Send for OnnxMiniLmEmbedder {}
#[cfg(not(feature = "onnx"))]
unsafe impl Sync for OnnxMiniLmEmbedder {}

#[cfg(not(feature = "onnx"))]
impl Embedder for OnnxMiniLmEmbedder {
    fn dim(&self) -> usize {
        MINILM_DIM
    }
    fn name(&self) -> &'static str {
        "onnx-minilm-l6-v2"
    }
    fn embed(&self, _text: &str) -> Vec<f32> {
        vec![0.0; MINILM_DIM]
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_hash_embedder_deterministic() {
        let embedder = HashEmbedder::new(128);
        let v1 = embedder.embed("write a code review");
        let v2 = embedder.embed("write a code review");

        assert_eq!(v1, v2, "Embedder deve essere deterministico");
        assert_eq!(v1.len(), 128);
    }

    #[test]
    fn test_hash_embedder_different_texts() {
        let embedder = HashEmbedder::new(128);
        let v1 = embedder.embed("review this code for bugs");
        let v2 = embedder.embed("generate documentation for api");

        assert_ne!(v1, v2, "Testi diversi devono avere embedding diversi");
    }

    #[test]
    fn test_embedder_normalized() {
        let embedder = HashEmbedder::new(64);
        let v = embedder.embed("test some text here");
        let norm: f32 = v.iter().map(|x| x * x).sum::<f32>().sqrt();
        assert!((norm - 1.0).abs() < 0.01 || norm == 0.0, "Vector deve essere L2-normalized, got norm={}", norm);
    }

    #[test]
    fn test_embedder_signature_distinguishes_models() {
        let hash = HashEmbedder::new(256);
        assert_eq!(hash.name(), "hash");
        assert_eq!(hash.signature(), "hash:256");

        // Signature diversa per dimensione diversa (stesso embedder)
        let hash384 = HashEmbedder::new(384);
        assert_ne!(hash.signature(), hash384.signature());

        // CachedEmbedder delega name/signature all'inner
        let cached = CachedEmbedder::new(HashEmbedder::new(256), 10);
        assert_eq!(cached.signature(), "hash:256");
    }

    #[test]
    fn test_cached_embedder() {
        let embedder = CachedEmbedder::new(HashEmbedder::new(64), 10);
        let text = "test caching";

        let v1 = embedder.embed(text);
        assert_eq!(embedder.cache_size(), 1);

        let v2 = embedder.embed(text);
        assert_eq!(v1, v2);
        assert_eq!(embedder.cache_size(), 1); // Cache hit, no new entry
    }
}

// ---------------------------------------------------------------------------
// Test di parita' numerica ONNX (Rust) vs SentenceTransformer (PyTorch).
// Prerequisito C6 dello studio brain->Rust: prima di sostituire l'embedder
// Python serve dimostrare che i vettori sono numericamente compatibili con
// quelli gia' indicizzati in Qdrant (altrimenti re-index obbligatorio).
//
// Riferimenti generati da Python: /tmp/minilm_ref_gen.py -> /tmp/minilm_ref.json
// Esecuzione (modello caricato dai path assoluti via env):
//   NEXUS_MINILM_MODEL=/home/administrator/ideai/models/minilm/model.onnx \
//   NEXUS_MINILM_TOKENIZER=/home/administrator/ideai/models/minilm/tokenizer.json \
//   cargo test -p nexus-orchestrator --features onnx minilm_parity -- --ignored --nocapture
// ---------------------------------------------------------------------------
#[cfg(all(test, feature = "onnx"))]
mod parity_tests {
    use super::*;

    fn cosine(a: &[f32], b: &[f32]) -> f32 {
        let dot: f32 = a.iter().zip(b).map(|(x, y)| x * y).sum();
        let na: f32 = a.iter().map(|x| x * x).sum::<f32>().sqrt();
        let nb: f32 = b.iter().map(|x| x * x).sum::<f32>().sqrt();
        dot / (na * nb + 1e-9)
    }

    #[test]
    #[ignore = "richiede /tmp/minilm_ref.json (generato da /tmp/minilm_ref_gen.py) + feature onnx"]
    fn minilm_parity_vs_pytorch() {
        let raw = std::fs::read_to_string("/tmp/minilm_ref.json")
            .expect("genera prima i riferimenti: python3 /tmp/minilm_ref_gen.py");
        let refs: Vec<serde_json::Value> = serde_json::from_str(&raw).expect("json valido");
        let emb = OnnxMiniLmEmbedder::try_from_env()
            .expect("OnnxMiniLmEmbedder: modello/tokenizer non caricati (controlla NEXUS_MINILM_*)");

        let mut min_cos = 1.0_f32;
        let mut sum_cos = 0.0_f32;
        for r in &refs {
            let text = r["text"].as_str().unwrap();
            let py: Vec<f32> = r["vec"]
                .as_array()
                .unwrap()
                .iter()
                .map(|x| x.as_f64().unwrap() as f32)
                .collect();
            let rs = emb.embed(text);
            assert_eq!(rs.len(), py.len(), "dim mismatch");
            let cos = cosine(&py, &rs);
            min_cos = min_cos.min(cos);
            sum_cos += cos;
            let preview: String = text.chars().take(38).collect();
            eprintln!("cos={cos:.6}  len={:<4} {preview:?}", text.len());
        }
        let mean = sum_cos / refs.len() as f32;
        eprintln!("--- MIN cosine={min_cos:.6}  MEAN cosine={mean:.6}  (n={}) ---", refs.len());
        assert!(
            min_cos > 0.999,
            "parita' insufficiente: min cosine {min_cos:.6} < 0.999 -> re-index Qdrant necessario o bug nel pooling/tokenizer"
        );
    }
}
