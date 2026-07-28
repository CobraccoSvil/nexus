//! `ui_surface`: PUNTO UNICO (regola L) della domanda «il lavoro appena fatto
//! ha toccato un'interfaccia?».
//!
//! Serve a decidere se il panel di review deve convocare anche la lente di
//! interfaccia. La risposta si legge dai FILE MODIFICATI — un fatto registrato
//! dal run — non dal testo del task ne' dalla prosa del modello (regola M): un
//! task che diceva "sistema le spese" puo' aver riscritto tre pagine, e uno che
//! diceva "rifai la dashboard" puo' aver toccato solo una query.
//!
//! Il vocabolario (suffissi e segmenti di percorso) e' un DATO nel DB, passato
//! come parametro: qui vive la REGOLA di riconoscimento, non l'elenco.

/// I file modificati toccano una superficie di interfaccia?
///
/// Due criteri, entrambi sul percorso normalizzato a minuscole con separatori
/// unificati:
/// - il file finisce con uno dei `suffissi` (`.tsx`, `.vue`, `.css`, ...);
/// - il percorso attraversa uno dei `segmenti` (`components`, `pages`, ...),
///   confrontato come COMPONENTE intero del percorso e non come sottostringa:
///   `src/components/x.ts` si', `src/decomponents.ts` no.
///
/// Vale il primo che risponde: un solo file di interfaccia basta a giustificare
/// la lente, perche' e' li' che l'utente guarda.
pub fn tocca_interfaccia(files: &[String], suffissi: &[String], segmenti: &[String]) -> bool {
    files.iter().any(|f| {
        let p = f.trim().to_lowercase().replace('\\', "/");
        if p.is_empty() {
            return false;
        }
        if suffissi
            .iter()
            .any(|s| !s.trim().is_empty() && p.ends_with(&s.trim().to_lowercase()))
        {
            return true;
        }
        let mut componenti = p.split('/');
        componenti.any(|c| {
            segmenti
                .iter()
                .any(|s| !s.trim().is_empty() && c == s.trim().to_lowercase())
        })
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn v(items: &[&str]) -> Vec<String> {
        items.iter().map(|s| s.to_string()).collect()
    }

    fn suffissi() -> Vec<String> {
        v(&[".tsx", ".jsx", ".vue", ".svelte", ".html", ".css", ".scss"])
    }

    fn segmenti() -> Vec<String> {
        v(&["components", "pages", "views", "screens"])
    }

    #[test]
    fn riconosce_dal_suffisso() {
        assert!(tocca_interfaccia(
            &v(&["src/lib/db.ts", "src/SpeseList.tsx"]),
            &suffissi(),
            &segmenti()
        ));
    }

    #[test]
    fn riconosce_dal_segmento_di_percorso() {
        assert!(tocca_interfaccia(
            &v(&["app/components/spese-table.ts"]),
            &suffissi(),
            &segmenti()
        ));
    }

    /// Il segmento si confronta come componente INTERO: a sottostringa
    /// `decomponents` e `pages_backup` accenderebbero la lente su codice che
    /// non e' interfaccia, e ogni review pagherebbe un revisore in piu'.
    #[test]
    fn il_segmento_non_scatta_dentro_unaltra_parola() {
        assert!(!tocca_interfaccia(
            &v(&["src/decomponents.ts", "src/pagesize.ts"]),
            &suffissi(),
            &segmenti()
        ));
    }

    #[test]
    fn nessun_file_di_interfaccia_nessuna_lente() {
        assert!(!tocca_interfaccia(
            &v(&["src/api/routes.ts", "migrations/0001.sql", "README.md"]),
            &suffissi(),
            &segmenti()
        ));
    }

    #[test]
    fn percorsi_windows_e_maiuscole() {
        assert!(tocca_interfaccia(
            &v(&["src\\Components\\Header.TSX"]),
            &suffissi(),
            &segmenti()
        ));
    }

    #[test]
    fn vocabolario_vuoto_non_riconosce_nulla() {
        assert!(!tocca_interfaccia(&v(&["a.tsx"]), &[], &[]));
        assert!(!tocca_interfaccia(&[], &suffissi(), &segmenti()));
        // Voci vuote nel vocabolario non devono far combaciare tutto.
        assert!(!tocca_interfaccia(
            &v(&["src/api/routes.ts"]),
            &v(&["", "  "]),
            &v(&["", " "])
        ));
    }
}
