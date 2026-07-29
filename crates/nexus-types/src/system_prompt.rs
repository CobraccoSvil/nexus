//! Composizione del system prompt: che cosa puo' precedere che cosa.
//!
//! ROOT CAUSE che ha reso necessario questo punto unico: un fornitore riusa il
//! prefisso di una richiesta solo se i primi token sono IDENTICI a quelli di una
//! richiesta gia' vista. In un run agentico l'executor ricompone il system a ogni
//! turno, e alcune direttive (il focus del turno, il razionale del piano)
//! venivano ANTEPOSTE: un blocco che cambia da un turno all'altro, messo in
//! testa, taglia il prefisso a tutto cio' che lo segue — cioe' a tutto il system.
//!
//! MISURATO il 29/07/2026 sul motore reale (`la_testa_del_prompt_resta_identica_fra_i_turni`):
//! fra il primo e il secondo turno dell'executor i caratteri in comune in testa
//! erano ZERO. Il blocco di focus compare al primo turno e sparisce al secondo,
//! perche' l'ultimo messaggio umano diventa un risultato di tool, i cui blocchi
//! `ToolResult` non portano testo. Sul ledger dei due giorni precedenti: 10,8M
//! token di prompt verso Mistral con l'11% servito da cache e $7,35 di costo,
//! contro DeepSeek che sugli STESSI run ne processa 12,1M al 64% di cache per
//! $1,27.
//!
//! Il criterio vive QUI e in un punto solo (regola L): un blocco dichiara di
//! essere ricalcolato a ogni turno, e il compositore lo mette DOPO la parte
//! stabile. Sparso nei call site, ogni nuova direttiva potrebbe tornare in testa
//! per distrazione e il difetto si riaprirebbe in silenzio — senza che nulla
//! fallisca, perche' un prompt con la testa instabile e' corretto in tutto
//! tranne che nel prezzo.
//!
//! Il confine e' un marcatore TESTUALE dentro il system, non un campo a parte:
//! cosi' viaggia da solo fino al gateway attraverso qualunque percorso, e nessun
//! call site deve ricordarsi di propagarlo. E' la stessa ragione per cui la
//! chiave di raggruppamento e' derivata dal prefisso invece che passata dai
//! chiamanti (vedi `prompt_cache_key` in `nexus-gateway`).

/// Confine fra la parte del system stabile per il run e quella ricalcolata a
/// ogni turno. Tutto cio' che segue questo marcatore e' per costruzione
/// variabile, e non entra nell'identita' del prefisso.
pub const CONFINE_DI_TURNO: &str = "[[NEXUS_SYSTEM_DI_TURNO]]";

/// Appende al system un blocco RICALCOLATO A OGNI TURNO, dietro il confine.
///
/// Un blocco vuoto e' un no-op: non introduce il confine, cosi' un turno senza
/// direttive resta bit-identico a un system senza parte variabile (e due turni,
/// uno con direttiva e uno senza, condividono comunque tutta la parte stabile).
///
/// Chiamate ripetute accodano al blocco esistente senza duplicare il confine:
/// l'ordine fra i blocchi di turno e' quello di iniezione.
pub fn appendi_blocco_di_turno(system: &str, blocco: &str) -> String {
    let blocco = blocco.trim();
    if blocco.is_empty() {
        return system.to_string();
    }
    let testa = system.trim_end();
    if system.contains(CONFINE_DI_TURNO) {
        format!("{testa}\n\n{blocco}")
    } else {
        format!("{testa}\n\n{CONFINE_DI_TURNO}\n{blocco}")
    }
}

/// Compone il system di un run: prima i blocchi che restano identici fra run
/// dello stesso progetto, poi quelli che cambiano da un run all'altro.
///
/// Stessa regola di [`appendi_blocco_di_turno`] a una granularita' piu' grossa.
/// Vale per il riuso CROSS-run: due run consecutivi sullo stesso progetto
/// condividono il prefisso fino al primo blocco che cambia, quindi ogni blocco
/// variabile anticipato accorcia il tratto riusabile di tutto cio' che lo segue.
/// Un blocco con lo stato dei servizi in seconda posizione rendeva inutile il
/// resto del system appena l'agente avviava un servizio.
///
/// Niente separatori aggiunti: i blocchi si isolano da soli (portano gia' i
/// propri a capo), come nella concatenazione che questa funzione sostituisce.
pub fn componi_system_di_run(stabili: &[&str], variabili_fra_run: &[&str]) -> String {
    let mut out = String::new();
    for parte in stabili.iter().chain(variabili_fra_run.iter()) {
        out.push_str(parte);
    }
    out
}

/// La parte del system che NON cambia da un turno all'altro: cio' che precede il
/// confine, oppure l'intero system se nessun blocco di turno e' stato appeso.
///
/// E' l'unica parte su cui ha senso costruire l'identita' di un prefisso: due
/// turni dello stesso run devono ottenere la stessa risposta da questa funzione,
/// altrimenti nessun raggruppamento tiene.
pub fn parte_stabile(system: &str) -> &str {
    match system.find(CONFINE_DI_TURNO) {
        Some(i) => system[..i].trim_end(),
        None => system,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const BASE: &str = "Sei l'agente di sviluppo. Lavora sul repository indicato.";

    #[test]
    fn il_blocco_di_turno_non_tocca_la_parte_stabile() {
        // Il contratto in una riga: due turni con direttive DIVERSE condividono
        // la stessa testa. E' la condizione perche' il fornitore riusi il
        // prefisso; se cade, ogni turno ripaga il prompt intero.
        let t1 = appendi_blocco_di_turno(BASE, "FOCUS: scrivi il file A");
        let t2 = appendi_blocco_di_turno(BASE, "FOCUS: ora correggi il file B");
        assert_ne!(t1, t2, "i due turni portano direttive diverse");
        assert_eq!(parte_stabile(&t1), parte_stabile(&t2));
        assert_eq!(parte_stabile(&t1), BASE);
    }

    #[test]
    fn il_turno_senza_direttiva_condivide_la_testa_con_quello_che_ce_l_ha() {
        // Il caso REALE che ha prodotto il difetto: la direttiva c'e' al primo
        // turno e sparisce al secondo (l'ultimo messaggio umano diventa un
        // risultato di tool). Prima questo bastava a perdere tutto il prefisso.
        let con = appendi_blocco_di_turno(BASE, "FOCUS: scrivi il file A");
        let senza = appendi_blocco_di_turno(BASE, "");
        assert_eq!(senza, BASE, "blocco vuoto: nessun confine, system invariato");
        assert_eq!(parte_stabile(&con), parte_stabile(&senza));
    }

    #[test]
    fn la_parte_stabile_cambia_se_cambia_il_system() {
        // Il verso opposto, che tiene onesti i test qui sopra: due system
        // diversi NON devono risultare lo stesso prefisso, altrimenti si
        // riuserebbe una testa che non e' la propria.
        let a = appendi_blocco_di_turno(BASE, "FOCUS: x");
        let b = appendi_blocco_di_turno("Istruzioni DIVERSE di progetto.", "FOCUS: x");
        assert_ne!(parte_stabile(&a), parte_stabile(&b));
    }

    #[test]
    fn piu_blocchi_di_turno_non_duplicano_il_confine() {
        let s = appendi_blocco_di_turno(BASE, "PRIMO");
        let s = appendi_blocco_di_turno(&s, "SECONDO");
        assert_eq!(s.matches(CONFINE_DI_TURNO).count(), 1);
        assert!(s.contains("PRIMO") && s.contains("SECONDO"));
        assert_eq!(parte_stabile(&s), BASE);
    }

    #[test]
    fn senza_confine_tutto_il_system_e_stabile() {
        assert_eq!(parte_stabile(BASE), BASE);
        assert_eq!(parte_stabile(""), "");
    }
}
