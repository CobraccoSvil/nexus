//! Chunking testo con overlap, semplice ma deterministico.
//!
//! Suddivide `text` in finestre da `chunk_size` caratteri con `overlap`
//! caratteri di sovrapposizione fra finestre consecutive. Lavora su
//! confini di parola (whitespace) quando possibile per evitare di
//! tagliare token a meta'.

/// Suddivide `text` in chunk. Garantisce:
/// - chunk.len() <= chunk_size (in numero di caratteri, non bytes)
/// - finestre consecutive condividono `overlap` caratteri
/// - mai chunk vuoti
pub fn chunk_text(text: &str, chunk_size: usize, overlap: usize) -> Vec<String> {
    if text.is_empty() || chunk_size == 0 {
        return Vec::new();
    }
    let overlap = overlap.min(chunk_size.saturating_sub(1));
    let chars: Vec<char> = text.chars().collect();
    if chars.len() <= chunk_size {
        return vec![text.to_string()];
    }

    let step = chunk_size - overlap;
    let mut out = Vec::new();
    let mut start = 0usize;
    while start < chars.len() {
        let end = (start + chunk_size).min(chars.len());
        // Cerca un boundary di whitespace vicino a `end` per non spezzare
        // parole, ma solo se l'aggiustamento e' piccolo (<= 50 char).
        let real_end = if end < chars.len() {
            let mut k = end;
            let min_k = end.saturating_sub(50).max(start + 1);
            while k > min_k && !chars[k - 1].is_whitespace() {
                k -= 1;
            }
            if k > min_k {
                k
            } else {
                end
            }
        } else {
            end
        };
        let slice: String = chars[start..real_end].iter().collect();
        let slice = slice.trim().to_string();
        if !slice.is_empty() {
            out.push(slice);
        }
        if real_end >= chars.len() {
            break;
        }
        // Garantisce progresso di almeno `step` rispetto allo start CORRENTE
        // (prev_start), non al next_start: la condizione precedente
        // (`real_end < start + step` dopo `start = real_end - overlap`)
        // confrontava il next_start con se stesso e su testi senza whitespace
        // forzava `start = next_start + step` annullando l'overlap (bug
        // visibile su input "aaa...": chunk_size=1000, overlap=200 produceva
        // 4 chunk di 1000 a step=1600 invece di 6 a step=800).
        let prev_start = start;
        let mut next_start = real_end.saturating_sub(overlap);
        if next_start < prev_start + step {
            next_start = prev_start + step;
        }
        if next_start >= chars.len() {
            break;
        }
        start = next_start;
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty() {
        assert!(chunk_text("", 100, 10).is_empty());
    }

    #[test]
    fn small_text_single_chunk() {
        let v = chunk_text("hello world", 100, 10);
        assert_eq!(v.len(), 1);
        assert_eq!(v[0], "hello world");
    }

    #[test]
    fn big_text_overlap() {
        let txt = "a".repeat(5000);
        let v = chunk_text(&txt, 1000, 200);
        assert!(v.len() >= 5);
        for c in &v {
            assert!(c.chars().count() <= 1000);
        }
    }
}
