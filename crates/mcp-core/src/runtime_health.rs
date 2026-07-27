//! Sentinella di SALUTE del runtime tokio: misura il RITARDO di risveglio.
//!
//! Perche' esiste (incidente consiglio 2026-07-15). Il task del consiglio e'
//! rimasto congelato ~287s: nessuno se n'e' accorto, e in due giorni la colpa
//! e' stata attribuita a rotazione a google (429), a deepseek (concorrenza),
//! al gateway ("non risponde") e al DB (una SELECT su chiave primaria che
//! "durava" 284s). Erano TUTTE deduzioni: il congelamento del runtime non
//! aveva alcun sensore, e i suoi sintomi somigliano a un guasto di chiunque
//! altro — perche' quando i task non vengono eseguiti, ogni attesa sembra
//! lentezza di cio' che si sta aspettando.
//!
//! Il principio (regola M): un `sleep(D)` che ritorna dopo `D + ritardo` e'
//! una MISURA diretta e strutturata del ritardo di scheduling, non
//! un'inferenza sul testo di un errore. Se il ritardo supera la soglia, il
//! runtime non stava eseguendo i task pronti: lo si dice, con un numero.
//!
//! Costo: un task che dorme; nessuna allocazione, nessuna query, nessun lock.

use std::time::{Duration, Instant};

use sqlx::PgPool;

/// Chiave settings (regola G, mig 0596) della soglia di allarme in ms.
const KEY_STARVATION_ALERT_MS: &str = "runtime.starvation_alert_ms";
/// Soglia se la riga manca. NON e' una soglia di comportamento del prodotto:
/// e' la sensibilita' di un sensore diagnostico (sotto i 2s su una macchina
/// carica ci sono falsi positivi legittimi, es. GC del sistema/antivirus).
const DEFAULT_STARVATION_ALERT_MS: u64 = 2000;
/// Periodo del battito: abbastanza fitto da campionare un congelamento di
/// pochi secondi, abbastanza rado da non pesare (2 poll/s).
const TICK: Duration = Duration::from_millis(500);

/// Ritardo di risveglio oltre il periodo atteso: la MISURA (pura, testabile).
/// `elapsed` = tempo realmente trascorso in un `sleep(tick)`.
pub(crate) fn wake_delay(elapsed: Duration, tick: Duration) -> Duration {
    elapsed.saturating_sub(tick)
}

/// `true` se il ritardo misurato merita l'allarme (pura, testabile).
pub(crate) fn is_starving(delay: Duration, threshold_ms: u64) -> bool {
    delay.as_millis() as u64 >= threshold_ms
}

/// Soglia dal DB (regola G), 0/assente/illeggibile -> default del sensore.
async fn threshold_ms(db: &PgPool) -> u64 {
    crate::settings::get_setting(db, KEY_STARVATION_ALERT_MS)
        .await
        .ok()
        .flatten()
        .and_then(|v| v.trim().parse::<u64>().ok())
        .filter(|n| *n > 0)
        .unwrap_or(DEFAULT_STARVATION_ALERT_MS)
}

/// Avvia la sentinella. Un solo task, per tutta la vita del processo.
pub fn spawn_runtime_health_sentinel(db: PgPool) {
    tokio::spawn(async move {
        // Cache della soglia: rileggerla a ogni battito sarebbe una query ogni
        // 500ms. 60s e' il TTL usato altrove nel repo per i settings caldi.
        let mut threshold = threshold_ms(&db).await;
        let mut last_reload = Instant::now();
        tracing::info!(
            target: "runtime_health",
            tick_ms = TICK.as_millis() as u64,
            threshold_ms = threshold,
            "runtime_health: sentinella avviata"
        );
        loop {
            let t0 = Instant::now();
            tokio::time::sleep(TICK).await;
            let delay = wake_delay(t0.elapsed(), TICK);
            if is_starving(delay, threshold) {
                // Il numero E' il segnale: quanto il runtime non ha eseguito i
                // task pronti. Chi legge non deve dedurlo da un timeout altrui.
                tracing::error!(
                    target: "runtime_health",
                    delay_ms = delay.as_millis() as u64,
                    threshold_ms = threshold,
                    "runtime_health: RUNTIME AFFAMATO — i task pronti non sono \
                     stati eseguiti per questo tempo. Sospettare un blocking \
                     sincrono dentro un task async (o un blocking pool saturo): \
                     i timeout scattano in ritardo e le attese I/O sembrano \
                     lentezza del servizio remoto."
                );
            }
            if last_reload.elapsed() >= Duration::from_secs(60) {
                threshold = threshold_ms(&db).await;
                last_reload = Instant::now();
            }
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn wake_delay_e_il_ritardo_oltre_il_periodo() {
        // Risveglio puntuale -> ritardo zero.
        assert_eq!(
            wake_delay(Duration::from_millis(500), TICK),
            Duration::ZERO
        );
        // Risveglio anticipato (clock) -> mai negativo.
        assert_eq!(
            wake_delay(Duration::from_millis(400), TICK),
            Duration::ZERO
        );
        // Il caso dell'incidente: sleep da 500ms tornato dopo 3s -> 2,5s di
        // ritardo, che e' esattamente cio' che il sensore deve dire.
        assert_eq!(
            wake_delay(Duration::from_millis(3000), TICK),
            Duration::from_millis(2500)
        );
    }

    #[test]
    fn is_starving_confronta_col_db() {
        assert!(!is_starving(Duration::from_millis(1999), 2000));
        assert!(is_starving(Duration::from_millis(2000), 2000));
        assert!(is_starving(Duration::from_millis(287_000), 2000));
        // Soglia piu' severa dal DB: lo stesso ritardo diventa allarme.
        assert!(is_starving(Duration::from_millis(600), 500));
    }

    #[sqlx::test]
    async fn soglia_dal_db_con_default(pool: sqlx::PgPool) {
        sqlx::query("CREATE TABLE settings (key TEXT PRIMARY KEY, value TEXT NOT NULL)")
            .execute(&pool)
            .await
            .expect("settings");
        assert_eq!(threshold_ms(&pool).await, DEFAULT_STARVATION_ALERT_MS);
        sqlx::query("INSERT INTO settings (key, value) VALUES ('runtime.starvation_alert_ms', '750')")
            .execute(&pool)
            .await
            .expect("insert");
        // Scrittura DIRETTA (fuori dal punto unico): la lettura e' cache-ata,
        // il test dichiara l'invalidazione come farebbe una sessione esterna.
        nexus_auth::invalidate_setting_cache(&pool, "runtime.starvation_alert_ms");
        assert_eq!(threshold_ms(&pool).await, 750, "il DB governa (regola G)");
        sqlx::query("UPDATE settings SET value = '0' WHERE key = 'runtime.starvation_alert_ms'")
            .execute(&pool)
            .await
            .expect("update");
        nexus_auth::invalidate_setting_cache(&pool, "runtime.starvation_alert_ms");
        assert_eq!(
            threshold_ms(&pool).await,
            DEFAULT_STARVATION_ALERT_MS,
            "0 disattiverebbe il sensore: si ricade sul default"
        );
    }

    /// IL TEST CHE CONTA (mutation-check di se stesso): con un blocking
    /// sincrono dentro un task async, la sentinella DEVE misurare un ritardo
    /// oltre soglia. Se questo test passasse senza il blocking, il sensore
    /// sarebbe cieco — lo stesso vizio di `verify_profile_missing`.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn la_sentinella_vede_il_blocking_sincrono() {
        // Occupa ENTRAMBI i worker con blocking sincrono: e' il caso reale
        // (blocking dentro task async, non spawn_blocking). I task partono al
        // primo punto di cessione di questo test, cioe' allo sleep qui sotto.
        for _ in 0..2 {
            tokio::spawn(async {
                std::thread::sleep(Duration::from_millis(1200));
            });
        }
        // t0 PRIMA di cedere: il timer scade a 500ms, ma nessun worker e'
        // libero per eseguirci fino a ~1200ms -> il ritardo e' proprio cio'
        // che il sensore deve misurare. (Prendere t0 dopo un'attesa
        // intermedia falserebbe la misura: quell'attesa e' gia' ritardata.)
        let t0 = Instant::now();
        tokio::time::sleep(TICK).await;
        let delay = wake_delay(t0.elapsed(), TICK);
        assert!(
            is_starving(delay, 300),
            "la sentinella NON ha visto il blocking (ritardo misurato: {}ms): \
             sensore cieco",
            delay.as_millis()
        );
    }
}
