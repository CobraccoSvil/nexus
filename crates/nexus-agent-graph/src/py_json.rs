//! `py_json`: serializzazione di un [`serde_json::Value`] BIT-IDENTICA a
//! `json.dumps(..., ensure_ascii=False)` di Python. PUNTO UNICO (regola L) della
//! serializzazione canonica Python-compatibile usata ai confini di parita' 1:1
//! col brain (signature anti-loop dell'executor, evidence dei criteri del
//! verifier). Stateless: una funzione pura, niente IO.
//!
//! ## Perche' un punto unico dedicato e non `serde_json::to_string`
//!
//! `serde_json::to_string` usa separatori COMPATTI (`,` / `:` senza spazi),
//! mentre `json.dumps` di Python usa di DEFAULT `", "` / `": "` (con SPAZIO).
//! Inoltre nel workspace la feature `preserve_order` di `serde_json` e' ATTIVA
//! (Cargo.toml root): un [`serde_json::Map`] e' un IndexMap che itera in ordine
//! d'INSERIMENTO, non alfabetico. Le due esigenze del brain:
//!   - `verifier_node` serializza l'evidence con `json.dumps(ensure_ascii=False)`
//!     SENZA `sort_keys` -> ordine d'inserimento ([`SortKeys::No`]);
//!   - `executor_node` costruisce la signature anti-loop con
//!     `json.dumps(input, sort_keys=True, ensure_ascii=False)` -> ordine
//!     ALFABETICO ricorsivo ([`SortKeys::Yes`]).
//!
//! ## Ordinamento delle chiavi (`sort_keys=True`)
//!
//! `sorted()` di Python ordina le stringhe per code point Unicode; `str::cmp`
//! di Rust ordina per byte UTF-8, e UTF-8 preserva l'ordine dei code point. Le
//! due relazioni d'ordine coincidono quindi sulle chiavi: ordinare con
//! `Vec::sort` (o `BTreeMap`) e' identico a `sorted()` Python. L'ordinamento e'
//! RICORSIVO (anche gli oggetti annidati).
//!
//! ## Separatori, stringhe, unicode
//!
//! - separatori `", "` fra elementi e `": "` fra chiave e valore;
//! - stringhe: virgolette + escape JSON standard, `ensure_ascii=False` -> i
//!   non-ASCII restano LETTERALI (UTF-8 nudo). `serde_json::to_string` su una
//!   `String` produce esattamente questo (niente `\uXXXX` per i non-ASCII).
//!
//! ## Numeri float: parita' con `float.__repr__`/`json.dumps` di CPython
//!
//! I numeri INTERI (`is_i64`/`is_u64`) si serializzano con `n.to_string()`
//! (es. `42` -> `"42"`): nessuna divergenza possibile col Python.
//!
//! I FLOAT invece NON possono usare `serde_json::Number::to_string()`: quella
//! via passa per la formattazione `{}` della stdlib Rust, che diverge da CPython
//! su due fronti:
//!   1. FORMATO: la soglia decimale/scientifica e il padding dell'esponente sono
//!      diversi (`1e-6` vs Python `1e-06`, `0.00001` vs Python `1e-05`,
//!      `6.022e23` vs Python `6.022e+23`);
//!   2. TIE-BREAK: la stdlib arrotonda i casi equidistanti round-half-AWAY,
//!      CPython round-half-to-EVEN (`-111275153569243.125` -> stdlib `...43.13`,
//!      Python `...43.12`).
//!
//! [`python_float_repr`] replica entrambi gli aspetti. Le cifre shortest con
//! tie-even le fornisce il crate `ryu` (che, a differenza della stdlib, usa
//! round-half-to-even come CPython); il FORMATO Python lo applica questo modulo:
//! notazione scientifica sse l'esponente decimale `x` soddisfa `x < -4 || x >=
//! 16`, altrimenti decimale; in scientifica esponente sempre con segno esplicito
//! e almeno 2 cifre; i float interi in decimale mantengono `.0`. Validato a 1:1
//! contro `json.dumps` reale di CPython (golden + fuzz su 200k+ valori).

use serde_json::Value;

/// Serializza un `f64` come `float.__repr__`/`json.dumps` di CPython.
///
/// Bit-identico al Python su: soglia decimale/scientifica (`x < -4 || x >= 16`,
/// dove `x` e' l'esponente decimale della prima cifra significativa), padding
/// dell'esponente (segno esplicito + almeno 2 cifre), `.0` sui float interi in
/// notazione decimale, e tie-break round-half-to-EVEN (delegato a `ryu`).
///
/// I valori `NaN`/`Infinity` non sono rappresentabili come [`serde_json::Number`]
/// valido, quindi non sono raggiungibili dal chiamante; per robustezza questa
/// funzione li mappa comunque ai letterali Python (`nan`/`inf`/`-inf`).
pub fn python_float_repr(f: f64) -> String {
    if f.is_nan() {
        return "nan".to_string();
    }
    if f.is_infinite() {
        return if f < 0.0 { "-inf" } else { "inf" }.to_string();
    }
    if f == 0.0 {
        // -0.0 mantiene il segno (json.dumps(-0.0) == "-0.0").
        return if f.is_sign_negative() {
            "-0.0".to_string()
        } else {
            "0.0".to_string()
        };
    }

    // Cifre shortest con tie-break round-half-to-even (== CPython). La stringa
    // di ryu usa pero' un FORMATO suo (soglie/esponente diversi da Python): la
    // scomponiamo in (segno, cifre significative, esponente decimale) e poi
    // applichiamo le regole di formato Python.
    let mut buf = ryu::Buffer::new();
    let (sign, digits, exp) = decompose_ryu(buf.format(f));

    if !(-4..16).contains(&exp) {
        // Notazione scientifica Python: mantissa shortest, "e", segno SEMPRE
        // presente, esponente con ALMENO 2 cifre (es. e-05, e+16, e+100).
        let mantissa = if digits.len() == 1 {
            digits.clone()
        } else {
            format!("{}.{}", &digits[..1], &digits[1..])
        };
        let esp_sign = if exp < 0 { '-' } else { '+' };
        let esp_abs = exp.unsigned_abs();
        return format!("{sign}{mantissa}e{esp_sign}{esp_abs:02}");
    }

    // Notazione decimale. `point_pos` = quante cifre stanno a sinistra del punto.
    let point_pos = exp + 1;
    let ndigits = digits.len() as i32;
    let mut out = String::with_capacity(digits.len() + 4);
    out.push_str(sign);
    if point_pos <= 0 {
        // 0.000ddd : zeri di guida fra il punto e le cifre significative.
        out.push_str("0.");
        for _ in 0..(-point_pos) {
            out.push('0');
        }
        out.push_str(&digits);
    } else if point_pos >= ndigits {
        // Tutte le cifre a sinistra, zeri di coda, poi ".0" (float intero).
        out.push_str(&digits);
        for _ in 0..(point_pos - ndigits) {
            out.push('0');
        }
        out.push_str(".0");
    } else {
        // Punto in mezzo alle cifre significative.
        let (l, r) = digits.split_at(point_pos as usize);
        out.push_str(l);
        out.push('.');
        out.push_str(r);
    }
    out
}

/// Scompone l'output shortest di `ryu` in `(segno, cifre_significative, exp)`,
/// dove `exp` e' l'esponente in base 10 della PRIMA cifra significativa.
///
/// Gestisce sia la forma decimale di ryu (`"0.0000123"`, `"1000000000000000.0"`)
/// sia quella scientifica (`"6.022e23"`, `"1.5e16"`). Esempi:
/// `"6.022e23"` -> `("", "6022", 23)`; `"0.5"` -> `("", "5", -1)`;
/// `"100.0"` -> `("", "1", 2)`; `"-1.5"` -> `("-", "15", 0)`.
fn decompose_ryu(s: &str) -> (&'static str, String, i32) {
    let (sign, rest) = if let Some(r) = s.strip_prefix('-') {
        ("-", r)
    } else {
        ("", s)
    };
    let (frac_str, e_exp) = match rest.split_once('e') {
        Some((m, e)) => (
            m,
            e.parse::<i32>()
                .expect("ryu emette sempre un esponente intero valido dopo 'e'"),
        ),
        None => (rest, 0),
    };
    let (int_part, frac_part) = match frac_str.split_once('.') {
        Some((i, f)) => (i, f),
        None => (frac_str, ""),
    };
    let all: String = format!("{int_part}{frac_part}");
    // Esponente della prima cifra di `all`: la prima cifra di int_part ha peso
    // 10^(int_part.len()-1 + e_exp).
    let mut exp_first = int_part.len() as i32 - 1 + e_exp;
    // Normalizza togliendo zeri di guida (aggiustando l'esponente) e di coda.
    let trimmed_lead = all.trim_start_matches('0');
    exp_first -= (all.len() - trimmed_lead.len()) as i32;
    let digits = trimmed_lead.trim_end_matches('0');
    let digits = if digits.is_empty() {
        // f != 0 garantito dal chiamante: non dovrebbe accadere, ma e' un
        // fallback sicuro che non panica.
        "0".to_string()
    } else {
        digits.to_string()
    };
    (sign, digits, exp_first)
}

/// Se ordinare alfabeticamente (ricorsivo) le chiavi degli oggetti, come
/// `json.dumps(sort_keys=...)` di Python.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SortKeys {
    /// `sort_keys=True`: chiavi ordinate alfabeticamente (per code point).
    Yes,
    /// `sort_keys=False` (default Python): ordine d'inserimento.
    No,
}

/// Serializza `v` come `json.dumps(v, ensure_ascii=False, sort_keys=<sort>)`.
///
/// Bit-identico al Python: separatori `", "`/`": "`, unicode letterale, e con
/// [`SortKeys::Yes`] chiavi ordinate alfabeticamente in modo ricorsivo.
pub fn py_json_dumps(v: &Value, sort: SortKeys) -> String {
    match v {
        Value::Null => "null".to_string(),
        Value::Bool(b) => {
            if *b {
                "true".to_string()
            } else {
                "false".to_string()
            }
        }
        Value::Number(n) => {
            // Interi: `to_string()` e' gia' bit-identico a Python (es. 42 ->
            // "42"). Float: serve il formato/tie-break di CPython, NON quello
            // della stdlib Rust su cui poggia Number::to_string (vedi modulo).
            if n.is_i64() || n.is_u64() {
                n.to_string()
            } else if let Some(f) = n.as_f64() {
                python_float_repr(f)
            } else {
                // serde_json con feature `arbitrary_precision`: n non e' ne
                // i64/u64 ne f64 rappresentabile. Non attivata nel workspace;
                // fallback alla stringa grezza preservata da serde.
                n.to_string()
            }
        }
        // json.dumps di una stringa con ensure_ascii=False: virgolette + escape
        // JSON standard, UTF-8 nudo. serde_json::to_string su una String produce
        // esattamente questo (niente \uXXXX per i non-ASCII di default).
        Value::String(_) => serde_json::to_string(v).unwrap_or_else(|_| "\"\"".into()),
        Value::Array(arr) => {
            let inner = arr
                .iter()
                .map(|e| py_json_dumps(e, sort))
                .collect::<Vec<_>>()
                .join(", ");
            format!("[{inner}]")
        }
        Value::Object(map) => {
            // Raccoglie le coppie e, con SortKeys::Yes, le ordina per chiave
            // (per byte UTF-8 == per code point == sorted() Python).
            let mut pairs: Vec<(&String, &Value)> = map.iter().collect();
            if sort == SortKeys::Yes {
                pairs.sort_by(|a, b| a.0.cmp(b.0));
            }
            let inner = pairs
                .into_iter()
                .map(|(k, val)| {
                    let key = serde_json::to_string(&Value::String(k.clone()))
                        .unwrap_or_else(|_| "\"\"".into());
                    format!("{key}: {}", py_json_dumps(val, sort))
                })
                .collect::<Vec<_>>()
                .join(", ");
            format!("{{{inner}}}")
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn separatori_con_spazio() {
        // json.dumps({"a": 1, "b": 2}) == '{"a": 1, "b": 2}'
        let v = json!({"a": 1, "b": 2});
        assert_eq!(py_json_dumps(&v, SortKeys::Yes), r#"{"a": 1, "b": 2}"#);
        // Array con separatore ", ".
        assert_eq!(py_json_dumps(&json!([1, 2, 3]), SortKeys::No), "[1, 2, 3]");
    }

    #[test]
    fn sort_keys_alfabetico_ricorsivo() {
        // Ordine d'inserimento sparso -> con SortKeys::Yes diventa alfabetico,
        // anche nell'oggetto annidato.
        let v = json!({"c": 1, "a": {"z": 1, "m": 2}, "b": 3});
        assert_eq!(
            py_json_dumps(&v, SortKeys::Yes),
            r#"{"a": {"m": 2, "z": 1}, "b": 3, "c": 1}"#
        );
    }

    #[test]
    fn sort_keys_no_preserva_inserimento() {
        // Con preserve_order attivo (workspace) e SortKeys::No: ordine d'insert.
        let v = json!({"c": 1, "a": 2, "b": 3});
        assert_eq!(
            py_json_dumps(&v, SortKeys::No),
            r#"{"c": 1, "a": 2, "b": 3}"#
        );
    }

    #[test]
    fn unicode_letterale() {
        // ensure_ascii=False: gli accenti restano letterali (UTF-8 nudo).
        let v = json!({"città": "caffè"});
        assert_eq!(py_json_dumps(&v, SortKeys::Yes), r#"{"città": "caffè"}"#);
    }

    #[test]
    fn python_float_repr_casi_tabellari() {
        // Tabella di divergenza Python json.dumps vs stdlib Rust. Atteso ==
        // json.dumps reale di CPython (verificato col golden + fuzz 200k).
        let casi: &[(f64, &str)] = &[
            // Esponente negativo: padding a 2 cifre, switch a scientifica.
            (1e-6, "1e-06"),
            (1e-7, "1e-07"),
            (1e-5, "1e-05"),
            (1.23e-5, "1.23e-05"),
            (1e-8, "1e-08"),
            (9.999e-5, "9.999e-05"),
            (2.5e-4, "0.00025"), // x = -4: NON < -4 -> decimale
            (1e-4, "0.0001"),    // confine: decimale
            (1e-100, "1e-100"),  // esponente a 3 cifre resta 3
            // Esponente positivo grande: segno '+' esplicito.
            (6.022e23, "6.022e+23"),
            (1e16, "1e+16"),  // x = 16: scientifica
            (1e15, "1000000000000000.0"), // x = 15: decimale
            (1.5e16, "1.5e+16"),
            (1e100, "1e+100"),
            (12345678901234567.0, "1.2345678901234568e+16"),
            // Decimali "umani".
            (1.0, "1.0"),
            (2.0, "2.0"),
            (0.5, "0.5"),
            (0.1, "0.1"),
            (100.0, "100.0"),
            (-1.5, "-1.5"),
            // Valore multi-cifra (non e' una costante nota a clippy).
            (3.732050807568877, "3.732050807568877"),
            (1234.5678, "1234.5678"),
            // Zeri con segno.
            (0.0, "0.0"),
            (-0.0, "-0.0"),
            // Tie-break round-half-to-EVEN (la stdlib Rust darebbe ...43.13 /
            // ...58.3). I valori esatti (...43.125 / ...58.25) hanno cifre oltre
            // la precisione f64: li costruiamo dai bit IEEE per evitare il lint
            // `excessive_precision` preservando il valore ESATTO.
            (f64::from_bits(0xc2d9_4d11_000d_76c8), "-111275153569243.12"),
            (f64::from_bits(0x431d_3a12_0ea6_a5e9), "2056655888558458.2"),
        ];
        for (f, expected) in casi {
            assert_eq!(
                python_float_repr(*f),
                *expected,
                "python_float_repr({f:?}) atteso {expected}"
            );
        }
    }

    #[test]
    fn float_dentro_py_json_dumps() {
        // I float esponenziali nel ramo Value::Number passano per
        // python_float_repr (signature/evidence bit-identiche al Python).
        let v = json!({"threshold": 1e-7, "x": 1.23e-5});
        assert_eq!(
            py_json_dumps(&v, SortKeys::Yes),
            r#"{"threshold": 1e-07, "x": 1.23e-05}"#
        );
        // Interi e float misti in un array.
        let v = json!({"mix": [1.0, 0.5, 100.0, 1e-8, 42]});
        assert_eq!(
            py_json_dumps(&v, SortKeys::No),
            r#"{"mix": [1.0, 0.5, 100.0, 1e-08, 42]}"#
        );
    }

    #[test]
    fn interi_invariati() {
        // Il path intero resta n.to_string(): nessuna regressione.
        assert_eq!(py_json_dumps(&json!(42), SortKeys::No), "42");
        assert_eq!(py_json_dumps(&json!(-7), SortKeys::No), "-7");
        assert_eq!(py_json_dumps(&json!(0), SortKeys::No), "0");
        assert_eq!(
            py_json_dumps(&json!(9007199254740993_i64), SortKeys::No),
            "9007199254740993"
        );
    }
}
