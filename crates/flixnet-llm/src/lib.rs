//! Optional LLM features over an OpenAI-compatible endpoint (Ollama, LM Studio,
//! OpenAI, …): natural language → browse filter, explanations grounded in the
//! viewer's own history, a tool-calling chat assistant, spoiler-free recaps and theme
//! collections.
//!
//! Everything here degrades gracefully: without a configured endpoint the callers
//! fall back to keyword search, rule-based explanations and plain episode overviews.
//! The LLM never sees more than the caller hands it — callers (flixnet-api) only pass
//! data the viewer may see, so parental controls and library access hold.

pub mod agent;
pub mod client;
pub mod explain;
pub mod prompts;
pub mod query;
pub mod recap;
pub mod semantic;
pub mod settings;
pub mod text;
pub mod themes;

pub use client::{LlmClient, LlmConfig, LlmError, Message, Role, ToolCall, ToolChoice, ToolSpec};
pub use settings::LlmSettings;
