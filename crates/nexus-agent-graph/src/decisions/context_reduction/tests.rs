//! Test unitari della parte PURA di context_reduction. La parita' 1:1 vs Python
//! e' nel modulo `golden`; qui si fissano i comportamenti chiave (idempotenza,
//! confini, no-op) leggibili senza il file golden.

use super::*;
use serde_json::json;

fn cfg_default() -> CtxMgmtConfig {
    CtxMgmtConfig {
        compress_start_iter: 5,
        compress_phase_boundaries: vec![5, 10, 20, 50],
        compress_phase_keep_recent: vec![8, 5, 3, 2],
        compress_phase_max_chars: vec![2000, 1000, 500, 150],
    }
}

/// Costruisce un messaggio con solo anthropic_content (blocchi).
fn msg_blocks(blocks: Value) -> HistoryMessage {
    HistoryMessage {
        is_human: false,
        content: Value::String(String::new()),
        anthropic_content: blocks,
        nexus_summary: false,
        rolling_summary: false,
        ..Default::default()
    }
}

fn human_text(text: &str) -> HistoryMessage {
    HistoryMessage {
        is_human: true,
        content: Value::String(text.to_string()),
        anthropic_content: Value::Null,
        nexus_summary: false,
        rolling_summary: false,
        ..Default::default()
    }
}

// ── 1) should_compress_now ──────────────────────────────────────────────────

#[test]
fn should_compress_fasi() {
    let cfg = cfg_default();
    // Sotto start: no.
    assert_eq!(
        should_compress_now(4, &cfg),
        (
            false,
            CompressParams {
                keep_recent: 0,
                max_content_chars: 0
            }
        )
    );
    // 5-9 -> idx 0 -> (8, 2000).
    assert_eq!(
        should_compress_now(5, &cfg).1,
        CompressParams {
            keep_recent: 8,
            max_content_chars: 2000
        }
    );
    assert_eq!(
        should_compress_now(9, &cfg).1,
        CompressParams {
            keep_recent: 8,
            max_content_chars: 2000
        }
    );
    // 10-19 -> idx 1 -> (5, 1000).
    assert_eq!(
        should_compress_now(10, &cfg).1,
        CompressParams {
            keep_recent: 5,
            max_content_chars: 1000
        }
    );
    // 20-49 -> idx 2 -> (3, 500).
    assert_eq!(
        should_compress_now(20, &cfg).1,
        CompressParams {
            keep_recent: 3,
            max_content_chars: 500
        }
    );
    // >=50 -> idx 3 -> (2, 150).
    assert_eq!(
        should_compress_now(50, &cfg).1,
        CompressParams {
            keep_recent: 2,
            max_content_chars: 150
        }
    );
    assert_eq!(
        should_compress_now(999, &cfg).1,
        CompressParams {
            keep_recent: 2,
            max_content_chars: 150
        }
    );
    // compress sempre true da start in poi.
    assert!(should_compress_now(5, &cfg).0);
}

// ── 2) dedup_tool_results_history ─────────────────────────────────────────────

#[test]
fn dedup_per_signature_tiene_ultimo() {
    // Due read_file con stessi args: il primo tool_result diventa placeholder.
    let tu = |id: &str| json!({ "type": "tool_use", "id": id, "name": "read_file", "input": {"path": "a.rs"} });
    let tr =
        |id: &str, body: &str| json!({ "type": "tool_result", "tool_use_id": id, "content": body });
    let msgs = vec![
        msg_blocks(json!([tu("t1")])),
        msg_blocks(json!([tr("t1", "primo contenuto")])),
        msg_blocks(json!([tu("t2")])),
        msg_blocks(json!([tr("t2", "secondo contenuto")])),
    ];
    let out = dedup_tool_results_history(&msgs);
    // Il tool_result t1 (non ultimo per la signature read_file|a.rs) -> placeholder.
    let b1 = out[1].anthropic_content.as_array().unwrap()[0].clone();
    assert_eq!(
        b1["content"].as_str().unwrap(),
        "[dedup: stesso tool con stessi args, vedi risultato piu' recente in msg #3]"
    );
    // t2 (ultimo) resta intatto.
    let b3 = out[3].anthropic_content.as_array().unwrap()[0].clone();
    assert_eq!(b3["content"].as_str().unwrap(), "secondo contenuto");
}

#[test]
fn dedup_no_op_se_signature_diverse() {
    let tu = |id: &str, path: &str| json!({ "type": "tool_use", "id": id, "name": "read_file", "input": {"path": path} });
    let tr = |id: &str| json!({ "type": "tool_result", "tool_use_id": id, "content": "x" });
    let msgs = vec![
        msg_blocks(json!([tu("t1", "a.rs")])),
        msg_blocks(json!([tr("t1")])),
        msg_blocks(json!([tu("t2", "b.rs")])),
        msg_blocks(json!([tr("t2")])),
    ];
    let out = dedup_tool_results_history(&msgs);
    assert_eq!(out, msgs);
}

// ── legacy dedup_tool_results (per content) ──────────────────────────────────

#[test]
fn dedup_legacy_per_content() {
    let big = "z".repeat(300);
    let tr =
        |id: &str, body: &str| json!({ "type": "tool_result", "tool_use_id": id, "content": body });
    let msgs = vec![
        msg_blocks(json!([tr("t1", &big)])),
        msg_blocks(json!([tr("t2", &big)])),
    ];
    let out = dedup_tool_results(&msgs);
    // Stesso content -> il primo (non ultimo) e' placeholder [deduped: ...].
    let b0 = out[0].anthropic_content.as_array().unwrap()[0].clone();
    assert_eq!(
        b0["content"].as_str().unwrap(),
        "[deduped: contenuto identico al tool_result piu' recente in msg #1]"
    );
    // content < 200 char: nessun dedup.
    let small = "k".repeat(100);
    let msgs2 = vec![
        msg_blocks(json!([tr("t1", &small)])),
        msg_blocks(json!([tr("t2", &small)])),
    ];
    assert_eq!(dedup_tool_results(&msgs2), msgs2);
}

// ── 3) looks_like_base64 / drop_unused_base64_payloads ───────────────────────

#[test]
fn looks_like_base64_euristica() {
    let b64 = "QUJDREVGR0hJSktMTU5PUFFSU1RVVldYWVowMTIzNDU2Nzg5".repeat(5);
    assert!(looks_like_base64(&b64, 200));
    // Stringa con newline nei primi min_len: no.
    let with_nl = format!("riga1\n{}", "A".repeat(300));
    assert!(!looks_like_base64(&with_nl, 200));
    // Troppo corta: no.
    assert!(!looks_like_base64("QUJD", 200));
    // Prosa normale (spazi/punteggiatura): sotto 90%.
    let prosa = "questo e' un testo normale con spazi e punteggiatura, niente base64.".repeat(5);
    assert!(!looks_like_base64(&prosa, 200));
}

#[test]
fn drop_base64_orfano_sostituito_referenziato_intatto() {
    let b64 = "QUJDREVGR0hJSktMTU5PUFFSU1RVVldYWVowMTIzNDU2Nzg5".repeat(5);
    let prefix16: String = b64.chars().take(16).collect();
    let tr =
        |id: &str, body: &str| json!({ "type": "tool_result", "tool_use_id": id, "content": body });
    // Caso orfano: 4 messaggi, max_age=3, keep_recent=2 -> boundary=2; il msg 0
    // ha base64 non citato nei successivi -> sostituito.
    let msgs = vec![
        msg_blocks(json!([tr("t1", &b64)])),
        human_text("nessuna citazione qui"),
        human_text("recente1"),
        human_text("recente2"),
    ];
    let out = drop_unused_base64_payloads(&msgs, 3, 2);
    assert!(out[0].anthropic_content.as_array().unwrap()[0]["content"]
        .as_str()
        .unwrap()
        .starts_with("[contenuto base64 originale di"));

    // Caso referenziato: il prefisso compare in un messaggio entro la finestra.
    let msgs2 = vec![
        msg_blocks(json!([tr("t1", &b64)])),
        human_text(&format!("riferimento al blob {prefix16} qui")),
        human_text("recente1"),
        human_text("recente2"),
    ];
    let out2 = drop_unused_base64_payloads(&msgs2, 3, 2);
    assert_eq!(
        out2[0].anthropic_content.as_array().unwrap()[0]["content"]
            .as_str()
            .unwrap(),
        b64
    );
}

// ── 4) compress_old_tool_results ─────────────────────────────────────────────

#[test]
fn compress_a_generazioni_sotto_cutoff() {
    let big = "y".repeat(1200);
    let tr =
        |id: &str, body: &str| json!({ "type": "tool_result", "tool_use_id": id, "content": body });
    let msgs = vec![
        msg_blocks(json!([tr("t1", &big)])), // i=0, sotto cutoff=1 -> compresso
        msg_blocks(json!([tr("t2", &big)])), // i=1, >= cutoff -> intatto
    ];
    let out = compress_old_tool_results(&msgs, 6, 500, Some(1), &degraded_marker);
    let c0 = out[0].anthropic_content.as_array().unwrap()[0]["content"]
        .as_str()
        .unwrap()
        .to_string();
    // kept = max(500/2, 100) = 250 char + marker degraded.
    assert!(c0.starts_with(&"y".repeat(250)));
    assert!(c0.ends_with(&format!(
        "[... compresso: {} char originali ...]",
        big.chars().count()
    )));
    // i=1 intatto.
    assert_eq!(
        out[1].anthropic_content.as_array().unwrap()[0]["content"]
            .as_str()
            .unwrap(),
        big
    );
}

#[test]
fn compress_cutoff_zero_no_op() {
    let msgs = vec![msg_blocks(
        json!([{ "type": "tool_result", "tool_use_id": "t", "content": "x".repeat(2000) }]),
    )];
    let out = compress_old_tool_results(&msgs, 6, 500, Some(0), &degraded_marker);
    assert_eq!(out, msgs);
}

#[test]
fn compress_sotto_soglia_non_tocca() {
    let small = "k".repeat(100);
    let msgs = vec![msg_blocks(
        json!([{ "type": "tool_result", "tool_use_id": "t", "content": small }]),
    )];
    let out = compress_old_tool_results(&msgs, 0, 500, Some(1), &degraded_marker);
    assert_eq!(out, msgs);
}

// ── 5) apply_token_brake ─────────────────────────────────────────────────────

#[test]
fn token_brake_sotto_soglia_no_op() {
    // Estimatore deterministico: somma dei char del content stringa.
    let est = |msgs: &[HistoryMessage]| -> i64 {
        msgs.iter()
            .map(|m| match &m.content {
                Value::String(s) => s.chars().count() as i64,
                _ => 0,
            })
            .sum()
    };
    let cfg = TokenBrakeConfig {
        max_context_ratio: 0.7,
        aggressive_keep_recent: 3,
        aggressive_max_chars: 200,
    };
    let msgs = vec![human_text("breve")];
    // window 1000 -> soglia 700; est=5 -> no-op.
    assert_eq!(apply_token_brake(&msgs, 1000, &cfg, &est), msgs);
}

#[test]
fn token_brake_comprime_aggressivo() {
    let est = |msgs: &[HistoryMessage]| -> i64 {
        msgs.iter()
            .map(|m| match &m.content {
                Value::String(s) => s.chars().count() as i64,
                _ => 0,
            })
            .sum()
    };
    let cfg = TokenBrakeConfig {
        max_context_ratio: 0.5,
        aggressive_keep_recent: 1,
        aggressive_max_chars: 50,
    };
    // 4 messaggi: primo human (preservato), 2 vecchi lunghi (troncati), 1 recente.
    let mut msgs = vec![
        human_text(&"a".repeat(100)), // first_human -> preservato
        human_text(&"b".repeat(400)), // vecchio -> troncato (ma e' human, non first)
        human_text(&"c".repeat(400)), // vecchio -> troncato
        human_text(&"d".repeat(50)),  // recente (keep_recent=1) -> preservato
    ];
    msgs[1].is_human = false;
    msgs[2].is_human = false;
    let out = apply_token_brake(&msgs, 200, &cfg, &est); // soglia 100
                                                         // Il first_human (i=0) resta intatto.
    assert_eq!(out[0].content.as_str().unwrap().chars().count(), 100);
    // I messaggi vecchi sono stati troncati (content piu' corto dell'originale).
    assert!(out[1].content.as_str().unwrap().chars().count() < 400);
}

// ── 6) inject_language_reminder ──────────────────────────────────────────────

#[test]
fn lang_reminder_idempotente() {
    let s = inject_language_reminder("SYSTEM BASE", true, "rispondi in italiano");
    assert!(s.starts_with(LANG_REMINDER_MARKER));
    assert!(s.contains("### LINGUA RISPOSTA OBBLIGATORIA ###"));
    assert!(s.contains("SYSTEM BASE"));
    // Doppia iniezione: testa + coda.
    assert_eq!(s.matches("rispondi in italiano").count(), 2);
    // Idempotente: ri-applicare non duplica.
    assert_eq!(
        inject_language_reminder(&s, true, "rispondi in italiano"),
        s
    );
    // Disabilitato o testo vuoto: no-op.
    assert_eq!(inject_language_reminder("X", false, "t"), "X");
    assert_eq!(inject_language_reminder("X", true, ""), "X");
}

// ── 7) inject_turn_focus (riuso marker + primitiva) ──────────────────────────

#[test]
fn turn_focus_idempotente_col_marker() {
    let msgs = vec![human_text("crea index.html")];
    let directive = build_turn_focus_directive(
        &[crate::state::Message::Human {
            content: crate::state::MessageContent::text("crea index.html"),
        }],
        false,
    )
    .expect("directive");
    let s1 = inject_turn_focus("SYS", &directive);
    assert!(s1.starts_with(TURN_FOCUS_MARKER));
    assert!(s1.contains("### FOCUS DEL TURNO CORRENTE ###"));
    // Idempotenza con marker gia' presente: NON re-iniettare.
    let s2 = inject_turn_focus(&s1, &directive);
    assert_eq!(s1, s2);
    // Directive vuota: no-op.
    assert_eq!(inject_turn_focus("SYS", ""), "SYS");
    let _ = msgs;
}

// ── 8a) inject_verification_directive ─────────────────────────────────────────

#[test]
fn verification_directive_condizioni() {
    let dir = "esegui la verifica reale";
    // detected + enabled: appende in coda con marker.
    let s = inject_verification_directive("SYS", true, true, dir);
    assert!(s.starts_with("SYS"));
    assert!(s.contains(VERIFY_DIRECTIVE_MARKER));
    assert!(s.contains("### AUTO-VERIFICA RICHIESTA DALL'UTENTE ###"));
    // Idempotente.
    assert_eq!(inject_verification_directive(&s, true, true, dir), s);
    // Non rilevato / disabilitato / vuoto: no-op.
    assert_eq!(
        inject_verification_directive("SYS", false, true, dir),
        "SYS"
    );
    assert_eq!(
        inject_verification_directive("SYS", true, false, dir),
        "SYS"
    );
    assert_eq!(inject_verification_directive("SYS", true, true, ""), "SYS");
}

// ── 8b) inject_forced_rag_reminder ────────────────────────────────────────────

#[test]
fn forced_rag_reminder_condizioni() {
    let msgs = vec![human_text("ciao")];
    // est >= ratio*window -> appende un messaggio in coda.
    let (out, sys) =
        inject_forced_rag_reminder(&msgs, "SYS", 80, 100, 0.5, "usa la ricerca semantica");
    assert_eq!(sys, "SYS"); // system intatto
    assert_eq!(out.len(), 2);
    assert!(out[1]
        .content
        .as_str()
        .unwrap()
        .contains(RAG_REMINDER_MARKER));
    // Idempotenza: ri-applicare sul risultato (marker negli ultimi 8) non aggiunge.
    let (out2, _) =
        inject_forced_rag_reminder(&out, "SYS", 80, 100, 0.5, "usa la ricerca semantica");
    assert_eq!(out2.len(), 2);
    // Sotto soglia / ratio<=0 / vuoto / window<=0: no-op.
    assert_eq!(
        inject_forced_rag_reminder(&msgs, "SYS", 10, 100, 0.5, "x")
            .0
            .len(),
        1
    );
    assert_eq!(
        inject_forced_rag_reminder(&msgs, "SYS", 80, 100, 0.0, "x")
            .0
            .len(),
        1
    );
    assert_eq!(
        inject_forced_rag_reminder(&msgs, "SYS", 80, 100, 0.5, "")
            .0
            .len(),
        1
    );
    assert_eq!(
        inject_forced_rag_reminder(&msgs, "SYS", 80, 0, 0.5, "x")
            .0
            .len(),
        1
    );
}

// ── 9) ROLLING SUMMARY (cutoff + serialize + apply) ───────────────────────────

/// Messaggio assistant testuale.
fn ai_text(text: &str) -> HistoryMessage {
    HistoryMessage {
        is_human: false,
        content: Value::String(text.to_string()),
        anthropic_content: Value::Null,
        ..Default::default()
    }
}

/// Messaggio `Message::Tool` (role=tool): is_tool=true + tool_call_id.
fn tool_msg(id: &str, body: &str) -> HistoryMessage {
    HistoryMessage {
        is_human: false,
        content: Value::String(body.to_string()),
        anthropic_content: Value::Null,
        is_tool: true,
        tool_call_id: Some(id.to_string()),
        ..Default::default()
    }
}

#[test]
fn rolling_cutoff_caso_normale() {
    // 5 messaggi, keep_recent=2 -> cutoff base = 3. Il msg[3] e' assistant (non
    // tool_result), quindi nessun aggiustamento: cutoff = 3.
    let hist = vec![
        human_text("domanda 1"),
        ai_text("risposta 1"),
        human_text("domanda 2"),
        ai_text("risposta 2"),
        human_text("domanda 3"),
    ];
    assert_eq!(select_rolling_summary_cutoff(&hist, 2), Some(3));
}

#[test]
fn rolling_cutoff_aggiusta_per_non_lasciare_tool_result_orfano() {
    // Sequenza: human, ai(tool_use), tool_result, ai, human (5 msg). keep_recent=2
    // -> cutoff base = 3, ma hist[3] e' assistant; controllo il caso in cui il
    // suffisso INIZIEREBBE con un tool_result orfano.
    // hist: [human, ai(tool_use), TOOL_RESULT, TOOL_RESULT, ai, human] (6 msg).
    // keep_recent=3 -> base = 3 -> hist[3] e' un tool_result orfano -> avanza a 4
    // (hist[4]=ai, non tool). cutoff = 4: i due tool_result finiscono nel prefisso.
    let hist = vec![
        human_text("domanda"),
        ai_text("uso un tool"),
        tool_msg("t1", "risultato tool 1"),
        tool_msg("t2", "risultato tool 2"),
        ai_text("ecco la risposta"),
        human_text("ok grazie"),
    ];
    let cut = select_rolling_summary_cutoff(&hist, 3).expect("cutoff Some");
    assert_eq!(
        cut, 4,
        "cutoff aggiustato per assorbire i tool_result nel prefisso"
    );
    // Il primo messaggio del suffisso NON e' un tool_result.
    assert!(
        !hist[cut].is_tool,
        "suffisso non parte con un tool_result orfano"
    );
}

#[test]
fn rolling_cutoff_none_history_corta() {
    // 2 messaggi, keep_recent=2 -> base = 0 -> None.
    let hist = vec![human_text("ciao"), ai_text("salve")];
    assert_eq!(select_rolling_summary_cutoff(&hist, 2), None);
    // 3 messaggi, keep_recent=2 -> base = 1 -> prefisso < MIN (2) -> None.
    let hist3 = vec![human_text("a"), ai_text("b"), human_text("c")];
    assert_eq!(select_rolling_summary_cutoff(&hist3, 2), None);
}

#[test]
fn rolling_cutoff_none_prefisso_tutto_summary() {
    // Prefisso gia' tutto messaggi summary -> niente di nuovo da riassumere.
    let mut s1 = human_text("[RIASSUNTO conversazione precedente]\nfatti");
    s1.rolling_summary = true;
    let mut s2 = human_text("[RIASSUNTO conversazione precedente]\naltri fatti");
    s2.rolling_summary = true;
    let hist = vec![s1, s2, human_text("nuova domanda"), ai_text("risposta")];
    // keep_recent=2 -> base = 2; hist[..2] e' tutto summary -> None.
    assert_eq!(select_rolling_summary_cutoff(&hist, 2), None);
}

#[test]
fn rolling_cutoff_none_se_tutto_suffisso_tool_result() {
    // Se dopo l'aggiustamento il cutoff supera la fine (tutto tool_result in coda)
    // -> None (nessun suffisso pulito residuo).
    let hist = vec![
        human_text("domanda"),
        ai_text("uso tool"),
        tool_msg("t1", "r1"),
        tool_msg("t2", "r2"),
    ];
    // keep_recent=2 -> base = 2 -> hist[2],hist[3] sono tool_result -> avanza a 4
    // = len -> None.
    assert_eq!(select_rolling_summary_cutoff(&hist, 2), None);
}

#[test]
fn rolling_serialize_prefix_testo_leggibile() {
    let hist = vec![
        human_text("come va il progetto?"),
        msg_blocks(json!([
            {"type": "text", "text": "controllo"},
            {"type": "tool_use", "id": "t1", "name": "read_file", "input": {"path": "a.rs"}}
        ])),
        tool_msg("t1", "contenuto del file"),
    ];
    let text = serialize_prefix_for_summary(&hist, 3);
    assert!(text.contains("[human]: come va il progetto?"));
    assert!(text.contains("[assistant]:"));
    assert!(text.contains("controllo"));
    assert!(text.contains("<tool read_file("));
    assert!(text.contains("[tool]: contenuto del file"));
    // Non e' JSON: niente parentesi graffe top-level di serializzazione messaggio.
    assert!(!text.trim_start().starts_with('{'));
}

#[test]
fn rolling_apply_collassa_e_preserva_suffisso() {
    let hist = vec![
        human_text("domanda 1"),
        ai_text("risposta 1"),
        human_text("domanda 2"),
        ai_text("risposta 2"),
        human_text("domanda 3"),
    ];
    let out = apply_rolling_summary(&hist, 3, "L'utente ha chiesto X, deciso Y.");
    // 3 collassati in 1 + 2 invariati = 3 messaggi.
    assert_eq!(out.len(), 3);
    // Primo messaggio: human di sintesi con marker e flag rolling_summary.
    assert!(out[0].is_human, "il primo messaggio resta human");
    assert!(out[0].rolling_summary, "flag rolling_summary attivo");
    assert!(!out[0].is_tool);
    assert!(out[0].tool_call_id.is_none());
    assert!(out[0].reasoning.is_none());
    let content = out[0].content.as_str().expect("content stringa");
    assert!(content.starts_with("[RIASSUNTO conversazione precedente]"));
    assert!(content.contains("deciso Y"));
    // Il riassunto e' riconosciuto come summary (preservato dalle riduzioni dopo).
    assert!(out[0].is_summary());
    // Suffisso invariato (gli stessi due messaggi originali).
    assert_eq!(out[1], hist[3]);
    assert_eq!(out[2], hist[4]);
}

// ── ADR 0016 D2: check_hard_cap / render_overflow_message ──────────────────

#[test]
fn hard_cap_scatta_sulla_soglia_esatta() {
    // window 1000, ratio 0.95 -> soglia 950: sotto no, uguale si, sopra si.
    assert!(!check_hard_cap(949, 1000, 0.95));
    assert!(check_hard_cap(950, 1000, 0.95));
    assert!(check_hard_cap(1200, 1000, 0.95));
}

#[test]
fn hard_cap_inerte_con_window_o_ratio_non_positivi() {
    // Default sicuro a config assente (DB down): gate disattivato.
    assert!(!check_hard_cap(10_000, 0, 0.95));
    assert!(!check_hard_cap(10_000, -1, 0.95));
    assert!(!check_hard_cap(10_000, 1000, 0.0));
    assert!(!check_hard_cap(10_000, 1000, -0.5));
}

#[test]
fn overflow_message_sostituisce_i_placeholder() {
    let template = "Stima: %ESTIMATED_TOKENS% token, finestra massima %MAX_WINDOW%.";
    let out = render_overflow_message(template, 1_200_000, 200_000);
    assert_eq!(out, "Stima: 1200000 token, finestra massima 200000.");
}

#[test]
fn overflow_message_senza_placeholder_resta_invariato() {
    let template = "Contesto oltre il limite configurato.";
    assert_eq!(
        render_overflow_message(template, 1, 2),
        "Contesto oltre il limite configurato."
    );
}

// ── 10) continuity-trim (coseno + selezione atomi + decisione + apply) ──────────

fn tool_use_msg(id: &str, name: &str) -> HistoryMessage {
    msg_blocks(json!([{ "type": "tool_use", "id": id, "name": name, "input": {} }]))
}
fn tool_result_msg(id: &str, body: &str) -> HistoryMessage {
    // Forma inline: primo blocco tool_result -> opens_with_tool_result = true.
    msg_blocks(json!([{ "type": "tool_result", "tool_use_id": id, "content": body }]))
}
fn assistant_text(text: &str) -> HistoryMessage {
    HistoryMessage {
        is_human: false,
        content: Value::String(text.to_string()),
        anthropic_content: Value::Null,
        ..Default::default()
    }
}

#[test]
fn cosine_similarity_casi_base() {
    // Identici -> 1.0; ortogonali -> 0.0; lunghezze diverse/vuoti/norma-nulla -> 0.0.
    assert!((cosine_similarity(&[1.0, 0.0], &[1.0, 0.0]) - 1.0).abs() < 1e-6);
    assert!(cosine_similarity(&[1.0, 0.0], &[0.0, 1.0]).abs() < 1e-6);
    assert_eq!(cosine_similarity(&[1.0, 2.0], &[1.0]), 0.0);
    assert_eq!(cosine_similarity(&[], &[]), 0.0);
    assert_eq!(cosine_similarity(&[0.0, 0.0], &[1.0, 1.0]), 0.0);
}

#[test]
fn select_continuity_candidates_atomi_e_confine() {
    // [human, assist(tool_use t1), tool_result t1, assist-text, keep_recent(2): assist, human]
    let hist = vec![
        human_text("task originale"),            // 0 human (ancora, escluso)
        tool_use_msg("t1", "read_file"),         // 1 \ atomo bilanciato droppabile [1,2]
        tool_result_msg("t1", "contenuto file"), // 2 /
        assistant_text("pensiero intermedio"),   // 3 assistant-text standalone [3]
        assistant_text("penultimo"),             // 4 keep_recent
        human_text("focus corrente"),            // 5 keep_recent
    ];
    // prefix_end = 6 - 2 = 4. Atomi candidati: [1,2] e [3]. Human 0 escluso.
    let cands = select_continuity_trim_candidates(&hist, 2);
    assert_eq!(cands.len(), 2);
    assert_eq!(cands[0].indices, vec![1, 2]);
    assert_eq!(cands[1].indices, vec![3]);
}

#[test]
fn select_continuity_candidates_tool_use_al_confine_non_droppabile() {
    // tool_use il cui tool_result cade nella coda keep_recent -> non candidato
    // (rischio orfano). prefix_end = 4 - 2 = 2.
    let hist = vec![
        human_text("task"),              // 0
        tool_use_msg("t1", "read_file"), // 1 tool_use, result a 2 (in coda) -> escluso
        tool_result_msg("t1", "body"),   // 2 keep_recent
        human_text("focus"),             // 3 keep_recent
    ];
    assert!(select_continuity_trim_candidates(&hist, 2).is_empty());
}

#[test]
fn decide_continuity_drops_scarta_sotto_soglia() {
    let candidates = vec![
        ContinuityCandidate {
            indices: vec![1, 2],
            text: "a".into(),
        },
        ContinuityCandidate {
            indices: vec![3],
            text: "b".into(),
        },
    ];
    let focus = vec![1.0f32, 0.0];
    // cand0 rilevante (coseno 1.0, >=0.5), cand1 irrilevante (coseno 0.0, <0.5).
    let cand_vecs = vec![vec![1.0f32, 0.0], vec![0.0f32, 1.0]];
    assert_eq!(
        decide_continuity_drops(&focus, &cand_vecs, &candidates, 0.5, 8),
        vec![3]
    );
}

#[test]
fn decide_continuity_drops_rispetta_il_cap() {
    let candidates = vec![
        ContinuityCandidate {
            indices: vec![1, 2],
            text: "a".into(),
        },
        ContinuityCandidate {
            indices: vec![3],
            text: "b".into(),
        },
    ];
    let focus = vec![1.0f32, 0.0];
    // Entrambi sotto soglia; cand1 (coseno -1.0) piu' irrilevante -> ordinato prima.
    // Cap=1: l'atomo [1,2] (2 msg) sfora, si scarta il singolo [3].
    let cand_vecs = vec![vec![0.0f32, 1.0], vec![-1.0f32, 0.0]];
    assert_eq!(
        decide_continuity_drops(&focus, &cand_vecs, &candidates, 0.5, 1),
        vec![3]
    );
}

#[test]
fn decide_continuity_drops_focus_vuoto_o_cap_zero_no_op() {
    let candidates = vec![ContinuityCandidate {
        indices: vec![1],
        text: "a".into(),
    }];
    let cand_vecs = vec![vec![0.0f32, 1.0]];
    assert!(decide_continuity_drops(&[], &cand_vecs, &candidates, 0.5, 8).is_empty());
    assert!(decide_continuity_drops(&[1.0, 0.0], &cand_vecs, &candidates, 0.5, 0).is_empty());
}

#[test]
fn apply_continuity_trim_rimuove_indici() {
    let hist = vec![
        human_text("0"),
        assistant_text("1"),
        assistant_text("2"),
        human_text("3"),
    ];
    let out = apply_continuity_trim(&hist, &[1, 2]);
    assert_eq!(out.len(), 2);
    assert_eq!(out[0].content, Value::String("0".into()));
    assert_eq!(out[1].content, Value::String("3".into()));
    // Nessun drop -> identita'.
    assert_eq!(apply_continuity_trim(&hist, &[]).len(), 4);
}

// ── 11) contents_eligible_for_offload ──────────────────────────────────────────

#[test]
fn contents_eligible_solo_tool_result_lunghi_sotto_cutoff() {
    let long = "x".repeat(50);
    let hist = vec![
        msg_blocks(
            json!([{ "type": "tool_result", "tool_use_id": "t1", "content": long.clone() }]),
        ), // 0 lungo, dentro cutoff
        msg_blocks(json!([{ "type": "tool_result", "tool_use_id": "t2", "content": "corto" }])), // 1 corto
        msg_blocks(
            json!([{ "type": "tool_result", "tool_use_id": "t3", "content": "y".repeat(50) }]),
        ), // 2 fuori cutoff
    ];
    // cutoff=2 (indici 0,1), threshold=10 -> solo il tool_result 0 (lungo) eleggibile.
    let eligible = contents_eligible_for_offload(&hist, 2, 10);
    assert_eq!(eligible, vec![long]);
}
