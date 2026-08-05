//! Rimozione di un blocco delimitato dal system prompt.
//!
//! Sta qui e non in `mcp-core::prompt_templates` perche' il suo consumatore e'
//! [`crate::ambiente`], che toglie il blocco `<privilegi_sistema>` quando il
//! gestore di pacchetti che quella direttiva ordina di usare non esiste sulla
//! macchina. Spostare il consumatore e lasciare qui la primitiva avrebbe
//! costretto il crate a dipendere da mcp-core, cioe' esattamente il verso che
//! l'estrazione toglie.

/// Il prompt senza il blocco compreso fra `inizio` e `fine` (estremi inclusi).
///
/// Se uno dei due delimitatori manca, il prompt torna **invariato**: un blocco
/// che non c'e' non e' un errore, e un `Option` costringerebbe ogni chiamante a
/// gestire un caso che per lui e' un no-op.
///
/// Il `trim_end_matches('\n')` sulla testa evita che la rimozione lasci una riga
/// vuota al posto del blocco: chi legge il prompt e' un modello, e una riga vuota
/// in piu' non e' gratis.
pub fn strip_block_between(prompt: &str, inizio: &str, fine: &str) -> String {
    let Some(start) = prompt.find(inizio) else {
        return prompt.to_string();
    };
    let Some(end_rel) = prompt[start..].find(fine) else {
        return prompt.to_string();
    };
    let end = start + end_rel + fine.len();
    let head = prompt[..start].trim_end_matches('\n');
    let mut out = String::with_capacity(head.len() + prompt.len().saturating_sub(end));
    out.push_str(head);
    out.push_str(&prompt[end..]);
    out
}

#[cfg(test)]
mod tests {
    use super::strip_block_between;

    #[test]
    fn il_blocco_sparisce_con_i_suoi_delimitatori() {
        let p = "testa\n<a>corpo</a>\ncoda";
        assert_eq!(strip_block_between(p, "<a>", "</a>"), "testa\ncoda");
    }

    #[test]
    fn un_delimitatore_mancante_lascia_il_prompt_intatto() {
        let p = "testa\n<a>corpo\ncoda";
        assert_eq!(strip_block_between(p, "<a>", "</a>"), p);
        assert_eq!(strip_block_between(p, "<mai>", "</mai>"), p);
    }

    #[test]
    fn la_rimozione_non_lascia_una_riga_vuota() {
        // Il caso reale: il blocco occupa una riga intera fra due altre.
        let p = "prima\n<x>y</x>\ndopo";
        let out = strip_block_between(p, "<x>", "</x>");
        assert!(!out.contains("\n\n"), "riga vuota residua in {out:?}");
    }
}
