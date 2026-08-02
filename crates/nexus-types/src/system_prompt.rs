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
/// Il CONFINE lo emette QUESTA funzione, sulla cucitura fra i due gruppi, appena
/// esiste una parte variabile non vuota. Fino al 02/08/2026 non lo emetteva
/// nessuno: il marcatore entrava solo se un blocco di turno veniva appeso DOPO
/// (in mcp-core, il solo blocco KB), quindi il confine dipendeva da un dato
/// variabile per messaggio — con la ricerca semantica a vuoto, o sotto
/// `min_score`, [`parte_stabile`] tornava a coprire l'INTERO system, memorie
/// comprese, e due richieste della stessa sessione finivano in gruppi di cache
/// diversi. Misurato dal test `tests_prefisso_fra_run` in mcp-core, nato ROSSO
/// su questo difetto: divergenza a 149 caratteri su 575 fra due run che
/// dovevano condividere tutta la testa.
///
/// L'ordine col ramo `contains` di [`appendi_blocco_di_turno`] e' quello
/// giusto per costruzione: i blocchi di TURNO appesi dopo si accodano dietro
/// questo stesso confine senza duplicarlo, e [`parte_stabile`] taglia al PRIMO
/// marcatore — cioe' sempre alla cucitura stabile/variabile del run.
///
/// Niente separatori aggiunti fra i blocchi: si isolano da soli (portano gia' i
/// propri a capo), come nella concatenazione che questa funzione sostituisce.
pub fn componi_system_di_run(stabili: &[&str], variabili_fra_run: &[&str]) -> String {
    let mut out = String::new();
    for parte in stabili.iter() {
        out.push_str(parte);
    }
    // Il confine si emette solo se esiste una parte variabile con del contenuto:
    // un run senza blocchi variabili resta bit-identico a ieri, e un system che
    // finisce col marcatore nudo non regala niente a nessuno.
    if variabili_fra_run.iter().any(|p| !p.trim().is_empty()) {
        let testa_pulita = out.trim_end().to_string();
        out = format!("{testa_pulita}\n\n{CONFINE_DI_TURNO}\n");
        for parte in variabili_fra_run.iter() {
            out.push_str(parte);
        }
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

/// Quanti caratteri iniziali due prompt hanno in comune: DOVE divergono, non
/// solo che divergono.
///
/// E' la misura con cui si legge un prefisso non riusato. Un'asserzione che
/// dice soltanto "le due teste sono diverse" lascia indovinare quale blocco si
/// e' messo davanti; il numero, letto insieme ai caratteri che seguono nei due
/// testi, nomina il punto esatto della divergenza — che e' anche il punto in
/// cui il fornitore smette di riusare (un numero senza la sua premessa e'
/// un'opinione, regola O).
///
/// Vive qui, accanto a [`parte_stabile`], perche' i due lati del confine si
/// misurano con lo stesso metro: i test del compositore di RUN (mcp-core,
/// `compose_agent_system_text`) e quelli del motore che ricompone il system a
/// ogni TURNO (`nexus-agent-graph`) pongono la stessa domanda, e due copie di
/// questo conteggio darebbero due idee diverse di "quanto e' comune".
///
/// Conta CARATTERI, non byte: il punto di divergenza si legge in un messaggio
/// di errore, e un indice di byte dentro un carattere multi-byte non e'
/// affettabile.
pub fn prefisso_comune(a: &str, b: &str) -> usize {
    a.chars().zip(b.chars()).take_while(|(x, y)| x == y).count()
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

    #[test]
    fn il_compositore_di_run_emette_il_confine_sulla_cucitura() {
        // Il contratto in una riga: la parte stabile di un system composto e'
        // ESATTAMENTE il gruppo degli stabili, qualunque cosa contengano i
        // variabili. Prima del fix il confine non veniva emesso da nessuno, e
        // parte_stabile copriva l'intero system — memorie comprese.
        //
        // MUTAZIONE: togliere l'emissione del confine (tornare alla pura
        // concatenazione) fa fallire la prima asserzione con la parte stabile
        // che ingloba "MEMORIE...", che e' il valore del difetto reale.
        let s = componi_system_di_run(
            &["PROGETTO: nexus\n", "Istruzioni.\n"],
            &["MEMORIE: solo di questo run\n"],
        );
        assert_eq!(parte_stabile(&s), "PROGETTO: nexus\nIstruzioni.");
        assert!(s.contains("MEMORIE: solo di questo run"));
        assert_eq!(s.matches(CONFINE_DI_TURNO).count(), 1);

        // Un blocco di TURNO appeso dopo si accoda dietro lo STESSO confine:
        // niente secondo marcatore, e la parte stabile non si muove.
        let con_turno = appendi_blocco_di_turno(&s, "FOCUS: il task di adesso");
        assert_eq!(con_turno.matches(CONFINE_DI_TURNO).count(), 1);
        assert_eq!(parte_stabile(&con_turno), "PROGETTO: nexus\nIstruzioni.");
    }

    #[test]
    fn senza_variabili_il_compositore_non_emette_il_confine() {
        // Un run senza parte variabile resta bit-identico alla concatenazione:
        // nessun marcatore nudo in coda, e tutta la testa e' stabile. Vale anche
        // per variabili presenti ma VUOTI (il caso reale: memorie a vuoto,
        // nessuna risorsa) — il confine condizionato alla PRESENZA del dato e
        // non al suo contenuto era esattamente il difetto.
        let solo_stabili = componi_system_di_run(&["PROGETTO: nexus\n"], &[]);
        assert!(!solo_stabili.contains(CONFINE_DI_TURNO));
        assert_eq!(parte_stabile(&solo_stabili), solo_stabili.as_str());

        let variabili_vuoti = componi_system_di_run(&["PROGETTO: nexus\n"], &["", "  \n"]);
        assert!(!variabili_vuoti.contains(CONFINE_DI_TURNO));
        assert_eq!(variabili_vuoti, solo_stabili);
    }

    #[test]
    fn il_prefisso_comune_dice_dove_si_diverge() {
        // Il servizio che rende: l'indice del primo carattere diverso, cioe' il
        // punto in cui il fornitore smette di riusare.
        assert_eq!(prefisso_comune("PROGETTO: nexus\nA", "PROGETTO: nexus\nB"), 16);
        assert_eq!(prefisso_comune("identici", "identici"), 8);
        assert_eq!(prefisso_comune("", "qualcosa"), 0);
        // Il piu' corto dei due limita il conteggio: nessun panic sul prefisso
        // proprio (un system troncato e' un caso reale, non un'ipotesi).
        assert_eq!(prefisso_comune("abc", "abcdef"), 3);
        // Caratteri, non byte: un accento conta uno.
        assert_eq!(prefisso_comune("perche' e' cosi'", "perche' e' colto"), 13);
    }
}
