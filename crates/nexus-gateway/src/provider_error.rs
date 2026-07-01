//! Errore tipizzato dei provider LLM e classificazione del cooldown a partire da
//! SEGNALI STRUTTURATI (status HTTP + `error_class` estratto dal body JSON del
//! provider), mai dal testo libero del messaggio (regola M).
//!
//! ## Perche' esiste
//!
//! Sul path di produzione (`server::routes::run_fallback` e lo stream SSE) gli
//! errori dei provider risalivano come `anyhow::Error` stringa e venivano
//! ri-classificati con `is_billing_error(err.to_string())`: una decisione di
//! stato tecnico presa sul TESTO gia' renderizzato ("provider HTTP 429: {body}"),
//! che confonde noise (nome provider, status) col segnale e non sa distinguere un
//! 429 "quota esaurita" (billing, cooldown lungo) da un 429 "rate-limit" (breve).
//!
//! [`ProviderError`] trasporta invece i segnali strutturati: lo `status` HTTP e
//! l'`error_class` (il campo `type`/`code`/`status` del body JSON d'errore). Il
//! classificatore [`ProviderError::cooldown_reason`] decide su QUELLI; il testo
//! del messaggio resta solo per Display/diagnostica.
//!
//! ## Punto unico (regola L)
//!
//! La classificazione billing/transient del gateway vive SOLO qui: `run_fallback`
//! e il path di streaming delegano entrambi a [`cooldown_reason_for`], senza
//! reimplementare lo split. La forma rispecchia il punto canonico
//! `mcp_core::brain_agent_client::classify_provider_error` (error_class primario,
//! marker testuali solo come fallback residuo): `nexus-gateway` e `mcp-core` sono
//! crate indipendenti senza edge di dipendenza, quindi la logica e' rispecchiata
//! nel medesimo vocabolario, non importata.
//!
//! ## Fallback residuo
//!
//! Alcuni provider non espongono un `error_class` dedicato al billing: Anthropic
//! segnala il credito esaurito con `type: "invalid_request_error"` (generico) e il
//! dettaglio SOLO nel messaggio. Per non regredire la detection, dopo i segnali
//! strutturati il classificatore consulta i marker testuali del punto unico
//! [`crate::providers::is_billing_error`] sul body del provider. E' l'identico
//! compromesso del classificatore canonico di mcp-core, documentato come tale.

use crate::cooldown::CooldownReason;

/// Errore di un provider LLM su risposta HTTP non-2xx. Porta i segnali
/// strutturati (`status`, `error_class`) usati per la classificazione del
/// cooldown; `message` e' il body grezzo del provider (privo di prompt/response,
/// regola F) tenuto per Display e come fallback residuo.
///
/// Il `Display` riproduce il formato storico `"{provider} HTTP {status}: {body}"`
/// cosi' che i consumatori a valle (la lista `failures` del 500, che il brain
/// legge) restino invariati.
#[derive(Debug, Clone, thiserror::Error)]
#[error("{provider} HTTP {status}: {message}")]
pub struct ProviderError {
    /// Nome canonico del provider che ha fallito (es. "openai", "anthropic").
    pub provider: String,
    /// Status HTTP della risposta d'errore.
    pub status: u16,
    /// `error_class` STRUTTURATO estratto dal body JSON (`error.type` /
    /// `error.code` / `error.status`). `None` se il body non e' JSON o non porta
    /// un codice: in tal caso la classificazione ricade su `status` e sul
    /// fallback residuo.
    pub error_class: Option<String>,
    /// Body grezzo del provider. SOLO per Display/diagnostica e per il fallback
    /// marker residuo; mai per decidere lo stato tecnico primario (regola M).
    pub message: String,
}

impl ProviderError {
    /// Costruisce l'errore da una risposta HTTP non-2xx, estraendo l'`error_class`
    /// strutturato dal body JSON. `body` e' il testo grezzo della risposta.
    pub fn from_http(provider: impl Into<String>, status: u16, body: impl Into<String>) -> Self {
        let message = body.into();
        let error_class = extract_error_class(&message);
        Self {
            provider: provider.into(),
            status,
            error_class,
            message,
        }
    }

    /// Motivo di cooldown dai segnali strutturati (status + `error_class`), con
    /// fallback residuo sul body per i provider senza `error_class` dedicato al
    /// billing. Punto unico della classificazione (regole L + M).
    pub fn cooldown_reason(&self) -> CooldownReason {
        // 1. Segnale strutturato: 402 Payment Required e' sempre billing.
        if self.status == 402 {
            return CooldownReason::Billing;
        }
        // 2. Segnale strutturato: error_class dedicato al billing dal body JSON.
        //    Copre il caso ambiguo del 429 (quota esaurita vs rate-limit): OpenAI
        //    ritorna `type: "insufficient_quota"` per la quota, `rate_limit_*` per
        //    il rate-limit, e la distinzione si legge sul CAMPO, non sul testo.
        if self
            .error_class
            .as_deref()
            .is_some_and(is_billing_error_class)
        {
            return CooldownReason::Billing;
        }
        // 3. Fallback RESIDUO: provider (Anthropic credit-balance) che segnalano il
        //    billing solo nel messaggio, con error_class generico. Marker nel punto
        //    unico `is_billing_error` (regola L).
        if crate::providers::is_billing_error(&self.message) {
            return CooldownReason::Billing;
        }
        CooldownReason::Transient
    }
}

/// Deriva il motivo di cooldown da un errore provider generico. Se e' un
/// [`ProviderError`] tipizzato usa i suoi segnali strutturati; altrimenti (errore
/// di trasporto/parsing: rete, timeout, JSON malformato) e' un transitorio.
/// Punto unico invocato dai call site (regole L + M): NON parsano il testo.
pub fn cooldown_reason_for(err: &anyhow::Error) -> CooldownReason {
    err.downcast_ref::<ProviderError>()
        .map(ProviderError::cooldown_reason)
        .unwrap_or(CooldownReason::Transient)
}

/// `error_class` strutturati che indicano billing/quota esaurita -> cooldown
/// lungo. Vocabolario allineato al punto canonico
/// `mcp_core::brain_agent_client::classify_provider_error` (regola L) e ai valori
/// `type`/`code` dei dialetti OpenAI/Anthropic.
fn is_billing_error_class(cls: &str) -> bool {
    matches!(
        cls,
        "insufficient_quota"
            | "billing_error"
            | "billing_required"
            | "quota_exceeded"
            | "credit_balance_too_low"
            | "billing_hard_limit_reached"
            | "payment_required"
    )
}

/// Estrae l'`error_class` STRUTTURATO dal body JSON d'errore leggendo i CAMPI
/// canonici dei dialetti provider: `error.type` (OpenAI/Anthropic), `error.status`
/// (Google, es. "RESOURCE_EXHAUSTED"), `error.code` (se stringa), o il `type`
/// top-level. Ritorna `None` se il body non e' JSON o non porta un codice. NB:
/// legge campi, NON fa substring matching sul messaggio (regola M).
fn extract_error_class(body: &str) -> Option<String> {
    let v: serde_json::Value = serde_json::from_str(body).ok()?;
    let from_error_obj = v.get("error").and_then(|e| {
        e.get("type")
            .and_then(serde_json::Value::as_str)
            .or_else(|| e.get("status").and_then(serde_json::Value::as_str))
            .or_else(|| e.get("code").and_then(serde_json::Value::as_str))
    });
    from_error_obj
        .or_else(|| v.get("type").and_then(serde_json::Value::as_str))
        .map(str::to_string)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extract_error_class_dialetto_openai() {
        // OpenAI: quota esaurita -> HTTP 429 con error.type = insufficient_quota.
        let body = r#"{"error":{"message":"You exceeded your current quota","type":"insufficient_quota","code":"insufficient_quota"}}"#;
        assert_eq!(extract_error_class(body).as_deref(), Some("insufficient_quota"));
    }

    #[test]
    fn extract_error_class_dialetto_anthropic() {
        let body = r#"{"type":"error","error":{"type":"rate_limit_error","message":"rate limited"}}"#;
        assert_eq!(extract_error_class(body).as_deref(), Some("rate_limit_error"));
    }

    #[test]
    fn extract_error_class_dialetto_google() {
        // Vertex/Gemini: error.status testuale (error.code e' numerico e va ignorato).
        let body = r#"{"error":{"code":429,"message":"Quota exceeded","status":"RESOURCE_EXHAUSTED"}}"#;
        assert_eq!(extract_error_class(body).as_deref(), Some("RESOURCE_EXHAUSTED"));
    }

    #[test]
    fn extract_error_class_body_non_json() {
        assert_eq!(extract_error_class("Bad Gateway"), None);
        assert_eq!(extract_error_class(""), None);
    }

    #[test]
    fn classifica_402_come_billing_da_status() {
        // Body senza error_class dedicato: e' il solo status a decidere.
        let e = ProviderError::from_http("deepseek", 402, "{\"error\":\"Insufficient Balance\"}");
        assert_eq!(e.cooldown_reason(), CooldownReason::Billing);
    }

    #[test]
    fn classifica_429_insufficient_quota_come_billing_da_error_class() {
        // 429 AMBIGUO: e' l'error_class strutturato (non il testo) a promuoverlo a
        // billing. Un boolean su testo non saprebbe distinguerlo da un rate-limit.
        let body = r#"{"error":{"type":"insufficient_quota","message":"quota"}}"#;
        let e = ProviderError::from_http("openai", 429, body);
        assert_eq!(e.error_class.as_deref(), Some("insufficient_quota"));
        assert_eq!(e.cooldown_reason(), CooldownReason::Billing);
    }

    #[test]
    fn classifica_429_rate_limit_come_transient() {
        let body = r#"{"error":{"type":"rate_limit_error","message":"slow down"}}"#;
        let e = ProviderError::from_http("anthropic", 429, body);
        assert_eq!(e.cooldown_reason(), CooldownReason::Transient);
    }

    #[test]
    fn classifica_5xx_come_transient() {
        let e = ProviderError::from_http("openai", 503, "service unavailable");
        assert_eq!(e.cooldown_reason(), CooldownReason::Transient);
    }

    #[test]
    fn classifica_anthropic_credit_balance_via_fallback_residuo() {
        // Anthropic: error.type generico (invalid_request_error), billing SOLO nel
        // messaggio -> il fallback residuo (is_billing_error) deve catturarlo.
        let body = r#"{"type":"error","error":{"type":"invalid_request_error","message":"Your credit balance is too low to access the Anthropic API"}}"#;
        let e = ProviderError::from_http("anthropic", 400, body);
        assert_eq!(e.error_class.as_deref(), Some("invalid_request_error"));
        assert_eq!(e.cooldown_reason(), CooldownReason::Billing);
    }

    #[test]
    fn classifica_google_resource_exhausted_come_transient() {
        // RESOURCE_EXHAUSTED non e' nel set billing: resta transitorio (rate-limit),
        // parita' col comportamento pre-esistente su Google.
        let body = r#"{"error":{"code":429,"message":"Quota exceeded","status":"RESOURCE_EXHAUSTED"}}"#;
        let e = ProviderError::from_http("google", 429, body);
        assert_eq!(e.cooldown_reason(), CooldownReason::Transient);
    }

    #[test]
    fn cooldown_reason_for_errore_non_tipizzato_e_transient() {
        // Errore di trasporto (non ProviderError) -> transitorio, senza toccare testo.
        let err = anyhow::anyhow!("connection reset by peer");
        assert_eq!(cooldown_reason_for(&err), CooldownReason::Transient);
    }

    #[test]
    fn cooldown_reason_for_provider_error_billing() {
        let err: anyhow::Error =
            ProviderError::from_http("openai", 402, "payment required").into();
        assert_eq!(cooldown_reason_for(&err), CooldownReason::Billing);
    }

    #[test]
    fn display_preserva_formato_storico() {
        let e = ProviderError::from_http("openai", 429, "quota exceeded body");
        assert_eq!(e.to_string(), "openai HTTP 429: quota exceeded body");
    }
}
