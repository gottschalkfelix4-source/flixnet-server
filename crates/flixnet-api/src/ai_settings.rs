//! `/api/v1/settings/llm` and `/api/v1/settings/scenes` (administrators): the
//! optional LLM endpoint and scene search (Whisper).

use axum::Json;
use axum::extract::State;
use flixnet_db::{libraries, transcripts};
use flixnet_library::scenes::{self as scene_jobs, SceneSettings};
use flixnet_llm::client::ChatRequest;
use flixnet_llm::settings::{self as llm_settings, LlmSettings, is_valid_base_url};
use flixnet_llm::{LlmError, Message};
use flixnet_media::whisper::{self, WhisperLanguage};
use serde::{Deserialize, Serialize};
use utoipa::ToSchema;
use utoipa_axum::router::OpenApiRouter;
use utoipa_axum::routes;

use crate::AppState;
use crate::auth::AdminUser;
use crate::error::{ApiError, ApiResult, ErrorBody};
use crate::validate;

pub(crate) fn router() -> OpenApiRouter<AppState> {
    OpenApiRouter::new()
        .routes(routes!(get_llm_settings, update_llm_settings))
        .routes(routes!(test_llm_settings))
        .routes(routes!(get_scene_settings, update_scene_settings))
}

// ---- LLM -------------------------------------------------------------------------------

#[derive(Debug, Serialize, ToSchema)]
pub struct LlmSettingsDto {
    pub enabled: bool,
    /// OpenAI-compatible API root, e.g. `http://localhost:11434/v1`.
    pub base_url: Option<String>,
    pub model: Option<String>,
    /// Whether an API key is stored (it is never returned).
    pub has_api_key: bool,
    pub timeout_secs: u64,
    /// Enabled and complete.
    pub configured: bool,
}

fn llm_dto(s: &LlmSettings) -> LlmSettingsDto {
    LlmSettingsDto {
        enabled: s.enabled,
        base_url: s.base_url.clone(),
        model: s.model.clone(),
        has_api_key: s.api_key.as_deref().is_some_and(|k| !k.is_empty()),
        timeout_secs: s.timeout_secs.unwrap_or(llm_settings::DEFAULT_TIMEOUT_SECS),
        configured: s.is_configured(),
    }
}

#[derive(Debug, Default, Deserialize, ToSchema)]
pub struct UpdateLlmSettings {
    pub enabled: Option<bool>,
    /// `null` removes it.
    #[serde(default, deserialize_with = "validate::double_option")]
    #[schema(value_type = Option<String>, nullable)]
    pub base_url: Option<Option<String>>,
    #[serde(default, deserialize_with = "validate::double_option")]
    #[schema(value_type = Option<String>, nullable)]
    pub model: Option<Option<String>>,
    /// `null` removes the stored key.
    #[serde(default, deserialize_with = "validate::double_option")]
    #[schema(value_type = Option<String>, nullable)]
    pub api_key: Option<Option<String>>,
    /// 5–900 seconds.
    pub timeout_secs: Option<u64>,
}

/// LLM endpoint settings.
#[utoipa::path(
    get,
    path = "/settings/llm",
    operation_id = "getLlmSettings",
    tag = "settings",
    responses(
        (status = OK, description = "Current settings", body = LlmSettingsDto),
        (status = UNAUTHORIZED, description = "Missing or invalid token", body = ErrorBody),
        (status = FORBIDDEN, description = "Not an administrator", body = ErrorBody),
    )
)]
async fn get_llm_settings(
    _: AdminUser,
    State(state): State<AppState>,
) -> ApiResult<Json<LlmSettingsDto>> {
    Ok(Json(llm_dto(&llm_settings::load(&state.db).await?)))
}

fn trimmed(value: Option<String>) -> Option<String> {
    value.map(|v| v.trim().to_owned()).filter(|v| !v.is_empty())
}

/// Change the LLM endpoint (Ollama, LM Studio, OpenAI, …).
#[utoipa::path(
    put,
    path = "/settings/llm",
    operation_id = "updateLlmSettings",
    tag = "settings",
    request_body = UpdateLlmSettings,
    responses(
        (status = OK, description = "Updated settings", body = LlmSettingsDto),
        (status = UNAUTHORIZED, description = "Missing or invalid token", body = ErrorBody),
        (status = FORBIDDEN, description = "Not an administrator", body = ErrorBody),
        (status = UNPROCESSABLE_ENTITY, description = "Invalid URL or timeout", body = ErrorBody),
    )
)]
async fn update_llm_settings(
    _: AdminUser,
    State(state): State<AppState>,
    Json(body): Json<UpdateLlmSettings>,
) -> ApiResult<Json<LlmSettingsDto>> {
    let mut s = llm_settings::load(&state.db).await?;
    if let Some(url) = body.base_url {
        let url = trimmed(url);
        if let Some(u) = &url
            && !is_valid_base_url(u)
        {
            return Err(ApiError::validation(
                "Die Adresse muss mit http:// oder https:// beginnen, z. B. http://localhost:11434/v1.",
            ));
        }
        s.base_url = url;
    }
    if let Some(model) = body.model {
        s.model = trimmed(model);
    }
    if let Some(key) = body.api_key {
        s.api_key = trimmed(key);
    }
    if let Some(timeout) = body.timeout_secs {
        if !(5..=900).contains(&timeout) {
            return Err(ApiError::validation(
                "Die Zeitgrenze muss zwischen 5 und 900 Sekunden liegen.",
            ));
        }
        s.timeout_secs = Some(timeout);
    }
    if let Some(enabled) = body.enabled {
        s.enabled = enabled;
    }
    if s.enabled && !s.is_configured() {
        return Err(ApiError::validation(
            "Zum Einschalten werden Adresse und Modell gebraucht.",
        ));
    }
    llm_settings::save(&state.db, &s).await?;
    Ok(Json(llm_dto(&s)))
}

#[derive(Debug, Serialize, ToSchema)]
pub struct LlmTestDto {
    pub ok: bool,
    /// The model's answer, or what went wrong.
    pub message: String,
    pub duration_ms: i64,
}

/// Send a tiny request to the configured endpoint.
#[utoipa::path(
    post,
    path = "/settings/llm/test",
    operation_id = "testLlmSettings",
    tag = "settings",
    responses(
        (status = OK, description = "Result of the test request", body = LlmTestDto),
        (status = UNAUTHORIZED, description = "Missing or invalid token", body = ErrorBody),
        (status = FORBIDDEN, description = "Not an administrator", body = ErrorBody),
        (status = CONFLICT, description = "Nothing configured", body = ErrorBody),
    )
)]
async fn test_llm_settings(
    _: AdminUser,
    State(state): State<AppState>,
) -> ApiResult<Json<LlmTestDto>> {
    let s = llm_settings::load(&state.db).await?;
    // Test even while switched off.
    let probe = LlmSettings { enabled: true, ..s };
    let Some(client) = probe.client() else {
        return Err(ApiError::conflict(
            "llm_not_configured",
            "Bitte zuerst Adresse und Modell eintragen.",
        ));
    };
    let started = flixnet_db::now_ms();
    let mut request = ChatRequest::new(vec![Message::user("Antworte nur mit dem Wort OK.")]);
    request.max_tokens = Some(20);
    let result = client.chat(&request).await;
    let duration_ms = flixnet_db::now_ms() - started;
    Ok(Json(match result {
        Ok(reply) => LlmTestDto {
            ok: true,
            message: flixnet_llm::client::strip_thinking(
                reply.content.as_deref().unwrap_or_default(),
            )
            .chars()
            .take(200)
            .collect(),
            duration_ms,
        },
        Err(err) => LlmTestDto {
            ok: false,
            message: match err {
                LlmError::Transport(_) => "Der Server ist nicht erreichbar.".to_owned(),
                LlmError::Timeout => "Keine Antwort innerhalb der Zeitgrenze.".to_owned(),
                LlmError::Http { status, message } => format!("Fehler {status}: {message}"),
                LlmError::Malformed(m) => format!("Unverständliche Antwort: {m}"),
            },
            duration_ms,
        },
    }))
}

// ---- Scene search ----------------------------------------------------------------------

#[derive(Debug, Serialize, ToSchema)]
pub struct WhisperModelDto {
    pub name: String,
    /// Download size in bytes.
    pub size: i64,
}

#[derive(Debug, Serialize, ToSchema)]
pub struct SceneLibraryDto {
    pub id: i64,
    pub name: String,
    /// Whisper transcribes files without subtitles in this library.
    pub transcribe: bool,
}

#[derive(Debug, Serialize, ToSchema)]
pub struct SceneSettingsDto {
    /// Path of the whisper.cpp CLI (`whisper-cli`); `null` = Whisper off.
    pub whisper_path: Option<String>,
    pub model: String,
    pub models: Vec<WhisperModelDto>,
    /// `auto` or an ISO 639-1 code.
    pub language: String,
    pub threads: u16,
    pub libraries: Vec<SceneLibraryDto>,
    /// Files with indexed dialogue.
    pub indexed_files: i64,
    pub chunks: i64,
    /// Files transcribed by Whisper.
    pub whisper_files: i64,
    /// Name of the task that (re)builds the index (`POST /tasks/{name}/run`).
    pub task: String,
}

#[derive(Debug, Default, Deserialize, ToSchema)]
pub struct UpdateSceneSettings {
    /// `null` switches Whisper off.
    #[serde(default, deserialize_with = "validate::double_option")]
    #[schema(value_type = Option<String>, nullable)]
    pub whisper_path: Option<Option<String>>,
    pub model: Option<String>,
    pub language: Option<String>,
    /// 1–64.
    pub threads: Option<u16>,
    /// Libraries to transcribe (replaces the current choice).
    pub transcribe_library_ids: Option<Vec<i64>>,
}

async fn scene_dto(state: &AppState, s: &SceneSettings) -> ApiResult<SceneSettingsDto> {
    let enabled = transcripts::transcribing_libraries(&state.db).await?;
    let stats = transcripts::stats(&state.db).await?;
    Ok(SceneSettingsDto {
        whisper_path: s.whisper_path.clone(),
        model: s.model.clone(),
        models: whisper::MODELS
            .iter()
            .map(|m| WhisperModelDto {
                name: m.name.to_owned(),
                size: m.size as i64,
            })
            .collect(),
        language: s.language.clone(),
        threads: s.threads,
        libraries: libraries::list(&state.db)
            .await?
            .into_iter()
            .map(|l| SceneLibraryDto {
                transcribe: enabled.contains(&l.id),
                id: l.id,
                name: l.name,
            })
            .collect(),
        indexed_files: stats.files,
        chunks: stats.chunks,
        whisper_files: stats.whisper_files,
        task: scene_jobs::TASK_NAME.to_owned(),
    })
}

/// Scene search settings and index size.
#[utoipa::path(
    get,
    path = "/settings/scenes",
    operation_id = "getSceneSettings",
    tag = "settings",
    responses(
        (status = OK, description = "Current settings", body = SceneSettingsDto),
        (status = UNAUTHORIZED, description = "Missing or invalid token", body = ErrorBody),
        (status = FORBIDDEN, description = "Not an administrator", body = ErrorBody),
    )
)]
async fn get_scene_settings(
    _: AdminUser,
    State(state): State<AppState>,
) -> ApiResult<Json<SceneSettingsDto>> {
    let s = scene_jobs::load_settings(&state.db).await?;
    Ok(Json(scene_dto(&state, &s).await?))
}

/// Change Whisper settings and which libraries are transcribed.
#[utoipa::path(
    put,
    path = "/settings/scenes",
    operation_id = "updateSceneSettings",
    tag = "settings",
    request_body = UpdateSceneSettings,
    responses(
        (status = OK, description = "Updated settings", body = SceneSettingsDto),
        (status = UNAUTHORIZED, description = "Missing or invalid token", body = ErrorBody),
        (status = FORBIDDEN, description = "Not an administrator", body = ErrorBody),
        (status = UNPROCESSABLE_ENTITY, description = "Invalid path, model, language or library", body = ErrorBody),
    )
)]
async fn update_scene_settings(
    _: AdminUser,
    State(state): State<AppState>,
    Json(body): Json<UpdateSceneSettings>,
) -> ApiResult<Json<SceneSettingsDto>> {
    let mut s = scene_jobs::load_settings(&state.db).await?;
    if let Some(path) = body.whisper_path {
        let path = trimmed(path);
        if let Some(p) = &path
            && !std::path::Path::new(p).is_file()
        {
            return Err(ApiError::validation(
                "Das Whisper-Programm wurde unter diesem Pfad nicht gefunden.",
            ));
        }
        s.whisper_path = path;
    }
    if let Some(model) = body.model {
        if whisper::model(&model).is_none() {
            return Err(ApiError::validation("Unbekanntes Whisper-Modell."));
        }
        s.model = model;
    }
    if let Some(language) = body.language {
        if WhisperLanguage::parse(&language).is_none() {
            return Err(ApiError::validation(
                "Sprache als zweistelligen Code (z. B. „de“) oder „auto“ angeben.",
            ));
        }
        s.language = if language.trim().is_empty() {
            "auto".to_owned()
        } else {
            language.trim().to_lowercase()
        };
    }
    if let Some(threads) = body.threads {
        if !(1..=64).contains(&threads) {
            return Err(ApiError::validation(
                "Die Anzahl der Threads muss zwischen 1 und 64 liegen.",
            ));
        }
        s.threads = threads;
    }
    if let Some(ids) = body.transcribe_library_ids {
        let all = libraries::list(&state.db).await?;
        if let Some(bad) = ids.iter().find(|id| !all.iter().any(|l| l.id == **id)) {
            return Err(ApiError::validation(format!(
                "Unbekannte Bibliothek {bad}."
            )));
        }
        for library in &all {
            transcripts::set_library_transcribes(&state.db, library.id, ids.contains(&library.id))
                .await?;
        }
    }
    flixnet_db::settings::set(&state.db, scene_jobs::SETTINGS_KEY, &s).await?;
    Ok(Json(scene_dto(&state, &s).await?))
}
