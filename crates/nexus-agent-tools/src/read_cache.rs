//! Cache LRU+TTL per deduplicare letture allegati ripetitive.
//!
//! Vedi ADR 0012 (FIX 2). Il loop osservato in produzione era:
//! il modello chiamava `nexus_read_archive_entry` 4+ volte con offset
//! progressivi sullo stesso `canvas.fig` binario, saturando il context
//! window. Questa cache:
//!
//! 1. Deduplica chiamate identiche (stessa attachment_id + entry_path +
//!    offset + length + encoding) servendo dalla memoria invece di rileggere
//!    il file. Il payload e' gia' serializzato in JSON (stringa pronta da
//!    ritornare al modello).
//! 2. Quando lo stesso pattern viene servito >= 2 volte, inietta un campo
//!    `from_cache`+`hint` per suggerire al modello di cambiare strategia
//!    (passare a un tool di estrazione strutturata invece di re-leggere
//!    chunk binari).
//!
//! TTL configurabile via setting DB `agent.attachment.read_cache_ttl_seconds`
//! (default 300 secondi). Capacita' fissa 256 entry (LRU evict).

use std::num::NonZeroUsize;
use std::sync::Arc;
use std::time::{Duration, Instant};

use lru::LruCache;
use once_cell::sync::Lazy;
use serde_json::{json, Value};
use tokio::sync::RwLock;
use uuid::Uuid;

use nexus_types::tool_outcome::RispostaTool;

use crate::attachment_settings;
use sqlx::PgPool;

/// Tipo di lettura per discriminare i namespace di cache.
#[derive(Debug, Clone, Copy, Hash, PartialEq, Eq)]
pub enum ReadKind {
    /// `nexus_read_attachment`: lettura range diretta dell'allegato.
    Attachment,
    /// `nexus_read_archive_entry`: lettura entry dentro archivio.
    ArchiveEntry,
}

#[derive(Debug, Clone, Hash, PartialEq, Eq)]
pub struct ReadCacheKey {
    pub attachment_id: Uuid,
    pub kind: ReadKind,
    pub entry_path: Option<String>,
    pub offset: u64,
    pub length: u64,
    pub encoding: String,
}

#[derive(Debug, Clone)]
struct CachedEntry {
    /// Payload JSON gia' serializzato (string pronta da ritornare al modello).
    /// Solo letture RIUSCITE: un fallimento non entra qui (vedi
    /// [`get_or_compute`]), quindi questo campo non porta mai un errore.
    payload: String,
    /// Numero di volte che questa entry e' stata servita (1 = prima scrittura).
    served_count: u32,
    /// Quando scade (Instant + TTL).
    expires_at: Instant,
}

const CAPACITY: usize = 256;

static READ_CACHE: Lazy<Arc<RwLock<LruCache<ReadCacheKey, CachedEntry>>>> = Lazy::new(|| {
    let cap = NonZeroUsize::new(CAPACITY).expect("capacita' read_cache > 0");
    Arc::new(RwLock::new(LruCache::new(cap)))
});

/// Ritorna il TTL letto da DB (cache 60s gia' applicata dal modulo
/// `attachment_settings`).
async fn cache_ttl(db: &PgPool) -> Duration {
    let limits = attachment_settings::current(db).await;
    Duration::from_secs(limits.read_cache_ttl_seconds.max(1) as u64)
}

/// Cerca la chiave nella cache. Se hit valido, incrementa `served_count` e
/// ritorna il payload arricchito con `from_cache`/`served_count`/`hint`.
/// Altrimenti chiama `compute` (lettura raw), memorizza il risultato SE e'
/// riuscito e lo ritorna.
///
/// # Solo i successi entrano in cache
///
/// Il payload memorizzato era una `String` opaca e la cache non aveva modo di
/// distinguere un contenuto letto da un errore: un fallimento — entry inesistente,
/// formato non riconosciuto, file che lo storage non consegna — veniva memorizzato
/// come qualunque altro payload e, alla seconda chiamata identica, riservito con
/// `from_cache: true` e l'invito a «cambiare strategia perche' leggere chunk
/// binari a offset crescenti non aggiunge informazione». Cioe' all'agente veniva
/// detto che stava ripetendo una lettura PRODUTTIVA proprio quando non stava
/// leggendo niente, e la causa radice — l'unica cosa da diagnosticare — spariva
/// dietro un suggerimento di anti-loop (regola M).
///
/// Col campo [`nexus_types::tool_outcome::EsitoTool`] la domanda diventa ponibile,
/// e la risposta e' che un fallimento non ha nulla da deduplicare: si ritorna
/// com'e', ogni volta, con la propria natura.
pub async fn get_or_compute<F, Fut>(db: &PgPool, key: ReadCacheKey, compute: F) -> RispostaTool
where
    F: FnOnce() -> Fut,
    Fut: std::future::Future<Output = RispostaTool>,
{
    // 1. Hit check con write lock (dobbiamo incrementare served_count).
    {
        let mut guard = READ_CACHE.write().await;
        if let Some(entry) = guard.get_mut(&key) {
            if Instant::now() < entry.expires_at {
                entry.served_count = entry.served_count.saturating_add(1);
                let served = entry.served_count;
                let base_payload = entry.payload.clone();
                drop(guard);
                // In cache ci sono solo successi: cio' che ne esce lo e'.
                return RispostaTool::riuscito(annotate_cached(base_payload, served));
            }
            // Scaduto: rimuovi e cadi su compute.
            guard.pop(&key);
        }
    }

    // 2. Miss: calcola e memorizza il solo successo.
    let risposta = compute().await;
    if risposta.esito.e_fallito() {
        return risposta;
    }
    let ttl = cache_ttl(db).await;
    let entry = CachedEntry {
        payload: risposta.testo.clone(),
        served_count: 1,
        expires_at: Instant::now() + ttl,
    };
    let mut guard = READ_CACHE.write().await;
    guard.put(key, entry);
    risposta
}

/// Aggiunge i campi `from_cache`/`served_count`/`hint` al payload JSON
/// originale quando la stessa richiesta e' gia' stata servita >= 2 volte.
fn annotate_cached(payload: String, served_count: u32) -> String {
    if served_count < 2 {
        return payload;
    }
    let mut value: Value = match serde_json::from_str(&payload) {
        Ok(v) => v,
        Err(_) => return payload,
    };
    if !value.is_object() {
        return payload;
    }
    let obj = value.as_object_mut().expect("valore JSON oggetto");
    obj.insert("from_cache".into(), json!(true));
    obj.insert("served_count".into(), json!(served_count));
    obj.insert(
        "hint".into(),
        json!(format!(
            "Questa esatta richiesta e' gia' stata servita {} volte.              Considera di passare a una entry diversa o di usare un tool di              estrazione strutturata (vedi extraction_tools/next_action_recommended              nell'inspector): leggere chunk binari a offset crescenti satura il              context window senza aggiungere informazione utile.",
            served_count
        )),
    );
    value.to_string()
}

#[cfg(test)]
pub async fn _reset_for_tests() {
    let mut guard = READ_CACHE.write().await;
    guard.clear();
}

#[cfg(test)]
mod tests {
    use super::*;
    use nexus_types::tool_outcome::{EsitoTool, NaturaFallimento};

    /// Pool MUTO: nessuna delle due prove ha un DB dietro. Il ramo del
    /// fallimento non lo tocca affatto — ritorna prima di [`cache_ttl`] — e
    /// quello del successo lo interroga per il solo TTL, che senza risposta
    /// ricade sui `safe_defaults` di [`crate::attachment_settings`].
    /// L'`acquire_timeout` corto e' li' per quella ricaduta: col default la
    /// prova pagherebbe 30 secondi per un valore che gia' sappiamo.
    fn pool_muto() -> PgPool {
        sqlx::postgres::PgPoolOptions::new()
            .acquire_timeout(Duration::from_millis(200))
            .connect_lazy("postgres://test:test@127.0.0.1:1/test")
            .expect("pool lazy")
    }

    /// Chiave NUOVA a ogni prova: la cache e' un globale di processo, e due
    /// test che se la dividessero dipenderebbero dall'ordine di esecuzione.
    fn chiave_nuova() -> ReadCacheKey {
        ReadCacheKey {
            attachment_id: Uuid::new_v4(),
            kind: ReadKind::Attachment,
            entry_path: None,
            offset: 0,
            length: 10,
            encoding: "auto".to_string(),
        }
    }

    /// Un FALLIMENTO non viene memorizzato: la chiamata identica successiva
    /// ricalcola invece di essere «servita».
    ///
    /// E' l'invariante per cui questa firma e' passata da `String` a
    /// [`RispostaTool`]: finche' il payload era una stringa opaca, un errore vi
    /// entrava come qualunque altro contenuto e alla seconda chiamata usciva con
    /// `from_cache`/`served_count` e l'invito a cambiare strategia — cioe' una
    /// lettura mai avvenuta veniva presentata come una ripetizione produttiva, e
    /// la causa radice spariva dietro un suggerimento di anti-loop (regola M).
    ///
    /// MUTAZIONE: togliendo il ritorno anticipato su `e_fallito()`, la seconda
    /// chiamata riceve il testo della PRIMA e questo test rosseggia.
    #[tokio::test]
    async fn un_fallimento_non_entra_in_cache() {
        let db = pool_muto();
        let key = chiave_nuova();

        let primo = get_or_compute(&db, key.clone(), || async {
            crate::errore_tool("entry 'a.txt' non trovata", NaturaFallimento::Rimediabile)
        })
        .await;
        assert_eq!(primo.esito, EsitoTool::Fallito, "{primo:?}");

        let secondo = get_or_compute(&db, key, || async {
            crate::errore_tool("secondo giro", NaturaFallimento::Rimediabile)
        })
        .await;
        assert!(
            secondo.testo.contains("secondo giro"),
            "il compute non e' rigirato: la risposta viene dalla cache: {secondo:?}"
        );
        assert!(
            !secondo.testo.contains("from_cache"),
            "un errore ripetuto e' una causa da diagnosticare, non una lettura \
             gia' servita: {secondo:?}"
        );
        assert_eq!(
            secondo.natura,
            Some(NaturaFallimento::Rimediabile),
            "la natura del SECONDO fallimento, non quella di una copia: {secondo:?}"
        );
    }

    /// La faccia opposta, che la stessa modifica poteva rompere in silenzio: un
    /// SUCCESSO resta deduplicato e alla seconda volta porta l'annotazione per
    /// cui la cache esiste. Ne esce un [`EsitoTool::Riuscito`] dichiarato nel
    /// campo, non una stringa da rileggere.
    ///
    /// MUTAZIONE: smettendo di memorizzare (o memorizzando sotto una chiave
    /// diversa), gira il secondo compute, `length` diventa 999 e il test
    /// rosseggia.
    #[tokio::test]
    async fn un_successo_resta_deduplicato_e_annotato() {
        let db = pool_muto();
        let key = chiave_nuova();

        let primo = get_or_compute(&db, key.clone(), || async {
            RispostaTool::riuscito(json!({"length": 3, "content": "abc"}).to_string())
        })
        .await;
        assert_eq!(primo.esito, EsitoTool::Riuscito, "{primo:?}");
        assert!(
            !primo.testo.contains("from_cache"),
            "la prima volta non e' una ripetizione: {primo:?}"
        );

        let secondo = get_or_compute(&db, key, || async {
            RispostaTool::riuscito(json!({"length": 999}).to_string())
        })
        .await;
        assert_eq!(secondo.esito, EsitoTool::Riuscito, "{secondo:?}");
        let corpo: Value = serde_json::from_str(&secondo.testo).expect("payload JSON integro");
        assert_eq!(
            corpo.get("length").and_then(Value::as_i64),
            Some(3),
            "servito dalla cache, non ricalcolato: {secondo:?}"
        );
        assert_eq!(corpo.get("from_cache"), Some(&json!(true)), "{secondo:?}");
        assert_eq!(
            corpo.get("served_count").and_then(Value::as_u64),
            Some(2),
            "{secondo:?}"
        );
    }
}
