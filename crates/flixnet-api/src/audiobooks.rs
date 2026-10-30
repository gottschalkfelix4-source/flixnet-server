//! `/api/v1/audiobooks*`: books of audiobook libraries, their parts and chapters, and
//! the listening position (one per user and book, across all parts).

use axum::Json;
use axum::extract::{Path, Query, State};
use flixnet_db::audiobooks::{self, BookProgress, BookQuery, BookRow};
use flixnet_db::{items, media};
use flixnet_library::audiobooks::{PartInfo, book_chapters, parts};
use serde::{Deserialize, Serialize};
use utoipa::{IntoParams, ToSchema};
use utoipa_axum::router::OpenApiRouter;
use utoipa_axum::routes;

use crate::AppState;
use crate::auth::AuthUser;
use crate::browse::visible_libraries;
use crate::error::{ApiError, ApiResult, ErrorBody};

pub(crate) fn router() -> OpenApiRouter<AppState> {
    OpenApiRouter::new()
        .routes(routes!(list_audiobooks))
        .routes(routes!(get_audiobook))
        .routes(routes!(save_audiobook_progress))
}

#[derive(Debug, Deserialize, IntoParams)]
#[into_params(parameter_in = Query)]
pub struct AudiobooksQuery {
    pub library_id: Option<i64>,
    /// Title, author or narrator contains this.
    pub search: Option<String>,
    /// 1–200, default 100.
    pub limit: Option<i64>,
    pub offset: Option<i64>,
}

#[derive(Debug, Serialize, ToSchema)]
pub struct AudiobookCardDto {
    pub id: i64,
    pub library_id: i64,
    pub title: String,
    pub year: Option<i64>,
    pub author: Option<String>,
    pub narrator: Option<String>,
    pub series_name: Option<String>,
    pub series_index: Option<f64>,
    /// Total duration of all parts.
    pub duration_ms: Option<i64>,
    /// Signed cover URL (342 px wide).
    pub cover_url: Option<String>,
    pub cover_blurhash: Option<String>,
    /// Listening position of the signed-in user (0 = not started or finished).
    pub position_ms: i64,
    /// Listened to the end.
    pub finished: bool,
    pub last_played_at: Option<i64>,
}

#[derive(Debug, Serialize, ToSchema)]
pub struct AudiobookPage {
    pub items: Vec<AudiobookCardDto>,
    pub total: i64,
    pub offset: i64,
    pub limit: i64,
}

#[derive(Debug, Serialize, ToSchema)]
pub struct AudiobookPartDto {
    pub file_id: i64,
    /// File name without extension.
    pub title: String,
    /// Signed URL of the audio file (supports range requests).
    pub url: String,
    /// Where the part starts in the book (ms).
    pub start_ms: i64,
    pub duration_ms: i64,
    pub container: Option<String>,
}

#[derive(Debug, Serialize, ToSchema)]
pub struct AudiobookChapterDto {
    pub title: String,
    /// Position in the whole book (ms).
    pub start_ms: i64,
    pub end_ms: i64,
    /// Index of the part (in `parts`) the chapter starts in.
    pub part: usize,
}

#[derive(Debug, Serialize, ToSchema)]
pub struct AudiobookDetailDto {
    #[serde(flatten)]
    pub card: AudiobookCardDto,
    pub overview: Option<String>,
    /// Signed cover URL (780 px wide).
    pub large_cover_url: Option<String>,
    /// The audio files in playback order.
    pub parts: Vec<AudiobookPartDto>,
    pub chapters: Vec<AudiobookChapterDto>,
}

fn card(state: &AppState, row: BookRow) -> AudiobookCardDto {
    AudiobookCardDto {
        cover_url: row
            .cover_image_id
            .map(|id| state.signer.image_url(id, Some(342))),
        cover_blurhash: row.cover_blurhash,
        id: row.id,
        library_id: row.library_id,
        title: row.title,
        year: row.year,
        author: row.author,
        narrator: row.narrator,
        series_name: row.series_name,
        series_index: row.series_index,
        duration_ms: row.runtime_ms,
        position_ms: row.position_ms,
        finished: row.played,
        last_played_at: row.last_played_at,
    }
}

/// Audiobooks, by author, series and title.
#[utoipa::path(
    get,
    path = "/audiobooks",
    operation_id = "listAudiobooks",
    tag = "audiobooks",
    params(AudiobooksQuery),
    responses(
        (status = OK, description = "One page of books", body = AudiobookPage),
        (status = UNAUTHORIZED, description = "Missing or invalid token", body = ErrorBody),
    )
)]
async fn list_audiobooks(
    user: AuthUser,
    State(state): State<AppState>,
    Query(query): Query<AudiobooksQuery>,
) -> ApiResult<Json<AudiobookPage>> {
    let q = BookQuery {
        library_ids: visible_libraries(&state, &user, query.library_id).await?,
        restriction: user.restriction.clone(),
        user_id: user.user_id,
        id: None,
        search: query.search,
        limit: query.limit.unwrap_or(100).clamp(1, 200),
        offset: query.offset.unwrap_or(0).max(0),
    };
    let (rows, total) = audiobooks::list(&state.db, &q).await?;
    Ok(Json(AudiobookPage {
        items: rows.into_iter().map(|r| card(&state, r)).collect(),
        total,
        offset: q.offset,
        limit: q.limit,
    }))
}

async fn visible_book(state: &AppState, user: &AuthUser, id: i64) -> ApiResult<BookRow> {
    let q = BookQuery {
        library_ids: visible_libraries(state, user, None).await?,
        restriction: user.restriction.clone(),
        user_id: user.user_id,
        id: Some(id),
        limit: 1,
        ..Default::default()
    };
    audiobooks::list(&state.db, &q)
        .await?
        .0
        .pop()
        .ok_or_else(|| ApiError::not_found("Hörbuch nicht gefunden."))
}

fn stem(path: &str) -> String {
    std::path::Path::new(path)
        .file_stem()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_default()
}

/// A book with its parts (signed URLs), chapters across all parts and the position.
#[utoipa::path(
    get,
    path = "/audiobooks/{id}",
    operation_id = "getAudiobook",
    tag = "audiobooks",
    params(("id" = i64, Path, description = "Item id of the book")),
    responses(
        (status = OK, description = "The book", body = AudiobookDetailDto),
        (status = UNAUTHORIZED, description = "Missing or invalid token", body = ErrorBody),
        (status = NOT_FOUND, description = "No such (visible) book", body = ErrorBody),
    )
)]
async fn get_audiobook(
    user: AuthUser,
    State(state): State<AppState>,
    Path(id): Path<i64>,
) -> ApiResult<Json<AudiobookDetailDto>> {
    let row = visible_book(&state, &user, id).await?;
    let large_cover_url = row
        .cover_image_id
        .map(|image| state.signer.image_url(image, Some(780)));
    let files = parts(&state.db, id).await.map_err(ApiError::internal)?;
    let mut infos = Vec::with_capacity(files.len());
    for file in &files {
        let chapters = media::chapters(&state.db, file.id)
            .await?
            .into_iter()
            .map(|c| (c.start_ms, c.end_ms, c.title))
            .collect();
        infos.push(PartInfo {
            file_id: file.id,
            duration_ms: file.duration_ms.unwrap_or(0),
            title: stem(&file.path),
            chapters,
        });
    }
    let (chapters, offsets, total) = book_chapters(&infos);
    let parts = files
        .iter()
        .zip(&infos)
        .zip(offsets)
        .map(|((file, info), start_ms)| AudiobookPartDto {
            file_id: file.id,
            title: info.title.clone(),
            url: state.signer.stream_url(file.id, user.user_id),
            start_ms,
            duration_ms: info.duration_ms,
            container: file.container.clone(),
        })
        .collect();
    let overview = items::details(&state.db, id)
        .await?
        .and_then(|d| d.overview);
    let mut card = card(&state, row);
    if total > 0 {
        card.duration_ms = Some(total);
    }
    Ok(Json(AudiobookDetailDto {
        card,
        overview,
        large_cover_url,
        parts,
        chapters: chapters
            .into_iter()
            .map(|c| AudiobookChapterDto {
                title: c.title,
                start_ms: c.start_ms,
                end_ms: c.end_ms,
                part: c.part,
            })
            .collect(),
    }))
}

#[derive(Debug, Deserialize, ToSchema)]
pub struct AudiobookProgressRequest {
    /// Position in the whole book (ms).
    pub position_ms: i64,
}

#[derive(Debug, Serialize, ToSchema)]
pub struct AudiobookProgressDto {
    /// Where the book resumes now (0 = from the start).
    pub position_ms: i64,
    /// Listened to the end.
    pub finished: bool,
}

/// Saves the listening position (every few seconds while playing and on pause).
#[utoipa::path(
    put,
    path = "/audiobooks/{id}/progress",
    operation_id = "saveAudiobookProgress",
    tag = "audiobooks",
    params(("id" = i64, Path, description = "Item id of the book")),
    request_body = AudiobookProgressRequest,
    responses(
        (status = OK, description = "Stored", body = AudiobookProgressDto),
        (status = UNAUTHORIZED, description = "Missing or invalid token", body = ErrorBody),
        (status = NOT_FOUND, description = "No such (visible) book", body = ErrorBody),
        (status = UNPROCESSABLE_ENTITY, description = "Negative position", body = ErrorBody),
    )
)]
async fn save_audiobook_progress(
    user: AuthUser,
    State(state): State<AppState>,
    Path(id): Path<i64>,
    Json(body): Json<AudiobookProgressRequest>,
) -> ApiResult<Json<AudiobookProgressDto>> {
    if body.position_ms < 0 {
        return Err(ApiError::validation(
            "Die Position darf nicht negativ sein.",
        ));
    }
    let row = visible_book(&state, &user, id).await?;
    let progress = audiobooks::save_progress(
        &state.db,
        user.user_id,
        id,
        body.position_ms,
        row.runtime_ms,
    )
    .await?;
    Ok(Json(match progress {
        BookProgress::Resume(position_ms) => AudiobookProgressDto {
            position_ms,
            finished: false,
        },
        BookProgress::Finished => AudiobookProgressDto {
            position_ms: 0,
            finished: true,
        },
        BookProgress::TooEarly => AudiobookProgressDto {
            position_ms: row.position_ms,
            finished: row.played,
        },
    }))
}
