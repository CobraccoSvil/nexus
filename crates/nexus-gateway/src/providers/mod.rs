//! Provider LLM concreti.
//!
//! Tutti i provider di questa Fase 2 parlano il dialetto OpenAI Chat
//! Completions e COMPONGONO il client condiviso [`openai_compat::OpenAiCompatClient`]
//! (regola L: punto unico, niente ereditarieta'). Ognuno fornisce la propria
//! `base_url`, `api_key` e capacita' (`max_context_tokens`, tier ammessi).

pub mod openai_compat;
pub mod tool_choice;
pub mod tool_error_channel;

pub mod anthropic;
pub mod deepseek;
pub mod gcp_auth;
pub mod generic;
pub mod google;
pub mod mistral;
pub mod openai;
pub mod vllm;

pub use anthropic::AnthropicProvider;
pub use deepseek::DeepSeekProvider;
pub use generic::GenericOpenAiProvider;
pub use google::GoogleProvider;
pub use mistral::MistralProvider;
pub use openai::OpenAiProvider;
pub use openai_compat::{
    classify_provider_error, OpenAiCompatClient, ProviderErrorKind, ProviderHttpError,
};
pub use vllm::VllmProvider;
