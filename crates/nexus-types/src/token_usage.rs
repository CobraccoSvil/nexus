//! Convenzione di ingresso dei token di prompt: il LORDO.
//!
//! PUNTO UNICO (regola L) della somma che porta al lordo. In tutto il sistema
//! `prompt_tokens` (`input_tokens` sul wire) e' il LORDO: comprende i token che
//! il provider ha servito dalla propria cache e quelli che ha scritto in cache.
//! E' la quantita' che quasi tutti i consumatori vogliono — quanto contesto e'
//! stato inviato, quanto e' piena la finestra, quanto e' cresciuta la history da
//! un turno all'altro — ed e' l'unica confrontabile fra turni e fra provider,
//! perche' la quota servita da cache la decide il provider e cambia da una
//! chiamata all'altra.
//!
//! I provider divergono su come lo riportano: quasi tutti contano la cache
//! DENTRO il prompt (nulla da fare), Anthropic la riporta a parte e il prompt
//! esce gia' al netto (qui si somma). La scelta di verso la fa l'adapter, che e'
//! l'unico a conoscere il formato che deserializza; la somma la fa questa
//! funzione, cosi' la convenzione ha un nome e un test invece di essere un ramo
//! di `match` senza titolo.
//!
//! Il NETTO — i soli token a tariffa piena di input — interessa a un solo
//! consumatore, il calcolo del costo, che lo scorpora al momento di tariffare
//! (`nexus_pricing::calculate_cost_breakdown`). Nessun altro deve vederlo.

/// Prompt LORDO: `input_tokens` + i token di cache.
///
/// Da chiamare SOLO sui provider che riportano il prompt al netto: per gli altri
/// il lordo e' gia' il numero del wire e sommare conterebbe due volte la cache.
/// Il verso lo decide l'unico chiamante odierno,
/// `nexus_gateway::LlmUsage::normalized`, sul segnale strutturato della
/// convenzione del provider (regola M).
///
/// `None` sulle quantita' di cache significa "il provider non le riporta", che
/// per la somma vale zero: non c'e' un terzo caso da distinguere qui — chi vuole
/// sapere se il provider le riporta guarda l'`Option`, non questo numero.
///
/// Somma satura: i tre addendi arrivano da un provider e un dato incoerente non
/// deve produrre un wrap a quattro miliardi di token.
pub fn prompt_tokens_gross(
    input_tokens: u32,
    cache_read_tokens: Option<u32>,
    cache_creation_tokens: Option<u32>,
) -> u32 {
    input_tokens
        .saturating_add(cache_read_tokens.unwrap_or(0))
        .saturating_add(cache_creation_tokens.unwrap_or(0))
}

#[cfg(test)]
mod tests {
    use super::prompt_tokens_gross;

    #[test]
    fn somma_le_tre_quantita_disgiunte() {
        assert_eq!(prompt_tokens_gross(1_000, Some(40_000), Some(500)), 41_500);
    }

    #[test]
    fn cache_non_riportata_vale_zero() {
        // Provider che non espone la cache: il lordo coincide col netto.
        assert_eq!(prompt_tokens_gross(1_200, None, None), 1_200);
    }

    #[test]
    fn somma_satura_invece_di_wrappare() {
        // Dato incoerente dal provider: meglio il tetto di u32 che un numero
        // piccolissimo ottenuto per wrap (che passerebbe per un prompt sano).
        assert_eq!(
            prompt_tokens_gross(u32::MAX, Some(10), Some(10)),
            u32::MAX,
            "la somma deve saturare"
        );
    }
}
