//! «Questo fornitore ha un tetto di spesa — e se lo superasse, chi lo
//! fermerebbe?»
//!
//! PUNTO UNICO (regola L) del criterio "esiste un tetto", e domanda ORTOGONALE
//! sia a [`crate::provider_readiness`] (sappiamo che risponde?) sia a
//! [`crate::provider_declaration`] (cio' che sappiamo basta a usarlo?). Un
//! fornitore puo' essere sano, dichiarato per intero, e spendere senza che
//! nessun presidio lo fermi: e' il caso reale misurato qui sotto.
//!
//! IL PRESIDIO E' UNO SOLO, E PRETENDE UN TETTO POSITIVO. `probe_one`
//! (`provider_health_probe`) e' l'unico punto che ferma un fornitore per
//! ragioni di spesa: mette in cooldown lungo con `budget_exhausted` quando
//! `(budget - spent) < min_threshold`. La sua query filtrava
//! `AND monthly_budget_usd > 0`, cioe' un fornitore senza tetto non veniva
//! nemmeno considerato. Non esiste un secondo presidio: MISURATO il 10/08/2026
//! sul META vivo, `ai_quota_policies` ha **0 righe** e `nexus_resource_quotas`
//! porta porte/memoria/disco per progetto, non spesa per fornitore.
//! Quindi: **nessuna riga di budget, o una riga con tetto 0, significa che
//! nessuno fermera' quel fornitore.**
//!
//! LA RIGA A TETTO ZERO NASCE DALLA SPESA STESSA. `charge_provider_budget`
//! (`chat_messages::agent_run`) fa `INSERT INTO provider_budget_status
//! (provider, spent_current_period_usd)`: `monthly_budget_usd` prende il suo
//! DEFAULT, che e' 0. Il primo addebito crea percio' una riga che sembra
//! configurata — c'e', in tabella — e che l'enforcement scarta e il pannello
//! nascondeva. Piu' un fornitore nuovo spende, piu' e' certo di avere la riga
//! che lo rende invisibile.
//!
//! MISURATO il 10/08/2026 sul META vivo, `ai_usage_ledger` degli ultimi 3
//! giorni incrociato con `provider_budget_status`:
//!
//! | fornitore  | chiamate | costo   | tetto |
//! |------------|----------|---------|-------|
//! | mistral    |      212 | $0.3358 | 20.00 |
//! | openrouter |       72 | $0.1540 |  0.00 |
//! | deepseek   |       58 | $0.0494 | 20.00 |
//! | kimi       |       29 | $0.1956 |  0.00 |
//! | google     |       17 | $0.0191 | 30.00 |
//! | groq       |       10 | $0.0018 |  0.00 |
//!
//! openrouter e kimi sono il SECONDO e il QUARTO fornitore per chiamate reali,
//! e sono esattamente due dei tre che nessuno fermerebbe.
//!
//! PERCHE' QUI NON SI INVENTA UN TETTO. Scrivere d'ufficio un numero — nel
//! codice, in una migrazione o a mano — produrrebbe un limite che *sembra* una
//! decisione dell'amministratore senza esserlo, e nel momento in cui mordesse
//! fermerebbe un fornitore per una soglia che nessuno ha scelto. Quale sia il
//! tetto giusto e' una decisione di chi paga; cio' che il codice deve garantire
//! e' che la sua ASSENZA abbia un nome e sia visibile. E' la stessa risposta
//! data per le capability mancanti (`provider_declaration`), e si usa lo stesso
//! meccanismo — un campo tipizzato sul wire, reso dalla UI — non un secondo.
//!
//! PERCHE' "tetto 0 deciso a mano" E "tetto mai deciso" NON SI DISTINGUONO, e
//! perche' non serve distinguerli per rispondere: `admin_set_provider_budget`
//! accetta 0, quindi in colonna i due casi sono lo stesso valore. Sono pero'
//! la stessa risposta alla domanda di questo modulo — in entrambi i casi
//! nessuno ferma quel fornitore — quindi il verdetto e' corretto senza una
//! colonna di provenienza. Distinguerli servirebbe a un'ALTRA domanda («chi ha
//! deciso questo tetto?»), che oggi nessuno pone.

/// Il verdetto sul tetto di spesa di un fornitore.
///
/// Regola Q: l'ignoto e' una variante dichiarata. `Undetermined` non degrada
/// ne' a "ha un tetto" (nasconderebbe il buco) ne' a "sta spendendo senza
/// tetto" (accuserebbe un fornitore su una misura che non abbiamo).
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum SpendCap {
    /// Tetto positivo: `probe_one` lo legge e ferma il fornitore quando il
    /// residuo scende sotto la soglia. `esaurito` e' il verdetto della vista.
    Capped { esaurito: bool },
    /// Nessun tetto, e una spesa gia' registrata: nessuno lo fermera'.
    UncappedSpending { spent_usd: f64 },
    /// Nessun tetto e nessuna spesa registrata: niente da fermare, per ora.
    UncappedIdle,
    /// Numeri non leggibili: non lo sappiamo.
    Undetermined,
}

impl SpendCap {
    /// Il valore canonico sul wire (regola N: identificatori in inglese).
    pub fn wire(&self) -> &'static str {
        match self {
            SpendCap::Capped { .. } => "capped",
            SpendCap::UncappedSpending { .. } => "uncapped_spending",
            SpendCap::UncappedIdle => "uncapped_idle",
            SpendCap::Undetermined => "undetermined",
        }
    }

    /// Esiste un tetto che l'enforcement possa applicare?
    ///
    /// E' il criterio che `provider_health_probe` esprimeva come
    /// `AND monthly_budget_usd > 0` dentro la propria query: vive qui perche'
    /// il pannello pone la STESSA domanda, e due `> 0` scritti in due posti
    /// sono due criteri che possono divergere.
    pub fn ha_tetto(&self) -> bool {
        matches!(self, SpendCap::Capped { .. })
    }

    /// Il fornitore va fermato ADESSO per ragioni di spesa.
    ///
    /// Solo un tetto che esiste puo' essere superato: senza tetto non c'e'
    /// esaurimento. La vista calcola `is_exhausted` anche a tetto 0 — li'
    /// `(0 - speso) < soglia` e' vero per costruzione — e prenderlo per buono
    /// fermerebbe ogni fornitore mai configurato al primo centesimo speso.
    pub fn ferma_adesso(&self) -> bool {
        matches!(self, SpendCap::Capped { esaurito: true })
    }

    /// Il caso richiede un intervento umano: sta spendendo e nessuno lo ferma.
    ///
    /// `UncappedIdle` NON lo richiede: un fornitore configurato che non ha
    /// ancora speso nulla non e' un problema, e segnalarlo renderebbe rumore
    /// la riga che conta.
    pub fn richiede_intervento(&self) -> bool {
        matches!(self, SpendCap::UncappedSpending { .. })
    }
}

/// Classifica il tetto dai numeri della riga di budget.
///
/// `None` = valore non leggibile (colonna assente, `NUMERIC::text` che non si
/// converte): non si specula, si dichiara `Undetermined`.
pub fn classifica(
    budget_usd: Option<f64>,
    spent_usd: Option<f64>,
    is_exhausted: bool,
) -> SpendCap {
    let (Some(budget), Some(spent)) = (budget_usd, spent_usd) else {
        return SpendCap::Undetermined;
    };
    if budget > 0.0 {
        return SpendCap::Capped {
            esaurito: is_exhausted,
        };
    }
    if spent > 0.0 {
        SpendCap::UncappedSpending { spent_usd: spent }
    } else {
        SpendCap::UncappedIdle
    }
}

/// Scrive il verdetto sull'entry JSON di un fornitore. Unico compositore
/// (regola L), come `provider_declaration::scrivi_dichiarazione`.
///
/// Il testo NON si compone qui (regola Q, punto 3): il wire porta il campo e la
/// UI ne fa una frase nella lingua dell'utente.
pub fn scrivi_tetto(p: &mut serde_json::Value, cap: &SpendCap) {
    p["spend_cap"] = serde_json::json!(cap.wire());
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tetto_positivo_e_governato_dall_enforcement() {
        let cap = classifica(Some(20.0), Some(3.0), false);
        assert_eq!(cap, SpendCap::Capped { esaurito: false });
        assert!(cap.ha_tetto());
        assert!(!cap.ferma_adesso());
        assert!(!cap.richiede_intervento());
        assert_eq!(cap.wire(), "capped");
    }

    #[test]
    fn tetto_positivo_ed_esaurito_ferma_adesso() {
        let cap = classifica(Some(20.0), Some(19.5), true);
        assert!(cap.ferma_adesso());
    }

    /// Il caso di openrouter e kimi, misurato il 10/08/2026.
    ///
    /// MUTAZIONE che lo fa rosseggiare: far ritornare `UncappedIdle` (o
    /// `Capped`) quando `budget == 0`, cioe' trattare l'assenza di tetto come
    /// un caso innocuo. `richiede_intervento()` torna false e l'assert cade.
    #[test]
    fn senza_tetto_ma_con_spesa_richiede_intervento() {
        let cap = classifica(Some(0.0), Some(16.99), false);
        assert_eq!(cap, SpendCap::UncappedSpending { spent_usd: 16.99 });
        assert!(!cap.ha_tetto());
        assert!(cap.richiede_intervento());
        assert_eq!(cap.wire(), "uncapped_spending");
    }

    /// La vista dice `is_exhausted = true` a tetto 0 perche' `(0 - speso) <
    /// soglia`: senza tetto non c'e' esaurimento, e prenderlo per buono
    /// fermerebbe il fornitore su una soglia che nessuno ha scelto.
    ///
    /// MUTAZIONE che lo fa rosseggiare: leggere `is_exhausted` prima di
    /// verificare che il tetto esista.
    #[test]
    fn senza_tetto_la_vista_dice_esaurito_ma_non_si_ferma_nessuno() {
        let cap = classifica(Some(0.0), Some(16.99), true);
        assert!(!cap.ferma_adesso());
        assert!(!cap.ha_tetto());
    }

    #[test]
    fn senza_tetto_e_senza_spesa_non_chiede_nulla() {
        let cap = classifica(Some(0.0), Some(0.0), false);
        assert_eq!(cap, SpendCap::UncappedIdle);
        assert!(!cap.richiede_intervento());
        assert_eq!(cap.wire(), "uncapped_idle");
    }

    /// Regola Q: l'ignoto non degrada verso nessuno dei due lati.
    #[test]
    fn numeri_non_leggibili_restano_ignoti() {
        for (b, s) in [(None, Some(1.0)), (Some(1.0), None), (None, None)] {
            let cap = classifica(b, s, false);
            assert_eq!(cap, SpendCap::Undetermined);
            assert!(!cap.ha_tetto(), "l'ignoto non promette un tetto");
            assert!(!cap.richiede_intervento(), "l'ignoto non accusa nessuno");
            assert!(!cap.ferma_adesso());
        }
    }

    #[test]
    fn il_wire_porta_il_campo() {
        let mut p = serde_json::json!({"provider": "kimi"});
        scrivi_tetto(&mut p, &classifica(Some(0.0), Some(0.19), false));
        assert_eq!(p["spend_cap"], "uncapped_spending");
    }
}
