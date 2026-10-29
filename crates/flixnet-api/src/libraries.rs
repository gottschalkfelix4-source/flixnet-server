//! `/api/v1/libraries` (media libraries) and `/api/v1/fs/directories` (folder picker).

use std::path::{Path, PathBuf};

use axum::Json;
use axum::extract::{Path as UrlPath, Query, State};
use axum::http::StatusCode;
use flixnet_core::jobs::priority;
use flixnet_db::libraries;
use flixnet_library::jobs::queue_scan;
use serde::{Deserialize, Serialize};
use utoipa::{IntoParams, ToSchema};
use utoipa_axum::router::OpenApiRouter;
use utoipa_axum::routes;

use crate::AppState;
use crate::auth::{AdminUser, AuthUser};
use crate::error::{ApiError, ApiResult, ErrorBody};

pub(crate) fn router() -> OpenApiRouter<AppState> {
    OpenApiRouter::new()
        .routes(routes!(list_libraries, create_library))
        .routes(routes!(get_library, update_library, delete_library))
        .routes(routes!(scan_library))
        .routes(routes!(list_directories))
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, Serialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum LibraryKind {
    Movies,
    Shows,
}

impl LibraryKind {
    fn as_str(self) -> &'static str {
        match self {
            LibraryKind::Movies => "movies",
            LibraryKind::Shows => "shows",
        }
    }
}

#[derive(Debug, Serialize, ToSchema)]
pub struct LibraryCountsDto {
    pub movies: i64,
    pub series: i64,
    pub episodes: i64,
    pub files: i64,
    /// Files whose technical details are still being read.
    pub unprobed: i64,
}

#[derive(Debug, Serialize, ToSchema)]
pub struct LibraryDto {
    pub id: i64,
    pub name: String,
    pub kind: String,
    /// Folder paths on the server (administrators only).
    pub paths: Option<Vec<String>>,
    pub counts: LibraryCountsDto,
    pub last_scan_at: Option<i64>,
}

#[derive(Debug, Deserialize, ToSchema)]
pub struct CreateLibraryRequest {
    pub name: String,
    pub kind: LibraryKind,
    /// Absolute folder paths on the server.
    pub paths: Vec<String>,
}

#[derive(Debug, Deserialize, ToSchema)]
pub struct UpdateLibraryRequest {
    pub name: Option<String>,
    pub paths: Option<Vec<String>>,
}

#[derive(Debug, Serialize, ToSchema)]
pub struct ScanQueued {
    /// Job id, or `null` if a scan of this library is already pending.
    pub job_id: Option<i64>,
}

#[derive(Debug, Deserialize, IntoParams)]
#[into_params(parameter_in = Query)]
pub struct DirectoriesQuery {
    /// Folder to list; empty lists the file system roots (drives on Windows).
    pub path: Option<String>,
}

#[derive(Debug, Serialize, ToSchema)]
pub struct DirectoryEntry {
    pub name: String,
    pub path: String,
}

#[derive(Debug, Serialize, ToSchema)]
pub struct DirectoryListing {
    /// The listed folder (`null` for the roots).
    pub path: Option<String>,
    /// Parent folder (`null` at a root).
    pub parent: Option<String>,
    pub directories: Vec<DirectoryEntry>,
}

/// Libraries the signed-in user may see.
#[utoipa::path(
    get,
    path = "/libraries",
    operation_id = "listLibraries",
    tag = "libraries",
    responses(
        (status = OK, description = "Visible libraries", body = Vec<LibraryDto>),
        (status = UNAUTHORIZED, description = "Missing or invalid token", body = ErrorBody),
    )
)]
async fn list_libraries(
    user: AuthUser,
    State(state): State<AppState>,
) -> ApiResult<Json<Vec<LibraryDto>>> {
    let list = libraries::list_for_user(&state.db, user.user_id).await?;
    let mut out = Vec::with_capacity(list.len());
    for library in list {
        out.push(to_dto(&state, library, user.is_admin).await?);
    }
    Ok(Json(out))
}

/// Create a library and start its first scan (administrators only).
#[utoipa::path(
    post,
    path = "/libraries",
    operation_id = "createLibrary",
    tag = "libraries",
    request_body = CreateLibraryRequest,
    responses(
        (status = CREATED, description = "Library created; scan queued", body = LibraryDto),
        (status = UNAUTHORIZED, description = "Missing or invalid token", body = ErrorBody),
        (status = FORBIDDEN, description = "Not an administrator", body = ErrorBody),
        (status = UNPROCESSABLE_ENTITY, description = "Invalid name or folders", body = ErrorBody),
    )
)]
async fn create_library(
    _: AdminUser,
    State(state): State<AppState>,
    Json(body): Json<CreateLibraryRequest>,
) -> ApiResult<(StatusCode, Json<LibraryDto>)> {
    let name = validate_name(&body.name)?;
    let paths = validate_paths(&body.paths).await?;
    let library = libraries::create(&state.db, &name, body.kind.as_str(), &paths).await?;
    queue_scan(state.jobs.queue(), library.id, priority::INTERACTIVE).await?;
    resync_watcher(&state).await;
    tracing::info!(library = %library.name, kind = %library.kind, "library created");
    Ok((
        StatusCode::CREATED,
        Json(to_dto(&state, library, true).await?),
    ))
}

/// One library.
#[utoipa::path(
    get,
    path = "/libraries/{id}",
    operation_id = "getLibrary",
    tag = "libraries",
    params(("id" = i64, Path, description = "Library id")),
    responses(
        (status = OK, description = "The library", body = LibraryDto),
        (status = UNAUTHORIZED, description = "Missing or invalid token", body = ErrorBody),
        (status = NOT_FOUND, description = "No such (visible) library", body = ErrorBody),
    )
)]
async fn get_library(
    user: AuthUser,
    State(state): State<AppState>,
    UrlPath(id): UrlPath<i64>,
) -> ApiResult<Json<LibraryDto>> {
    let library = visible_library(&state, &user, id).await?;
    Ok(Json(to_dto(&state, library, user.is_admin).await?))
}

/// Rename a library or change its folders (administrators only).
#[utoipa::path(
    patch,
    path = "/libraries/{id}",
    operation_id = "updateLibrary",
    tag = "libraries",
    params(("id" = i64, Path, description = "Library id")),
    request_body = UpdateLibraryRequest,
    responses(
        (status = OK, description = "Updated library", body = LibraryDto),
        (status = UNAUTHORIZED, description = "Missing or invalid token", body = ErrorBody),
        (status = FORBIDDEN, description = "Not an administrator", body = ErrorBody),
        (status = NOT_FOUND, description = "No such library", body = ErrorBody),
        (status = UNPROCESSABLE_ENTITY, description = "Invalid name or folders", body = ErrorBody),
    )
)]
async fn update_library(
    _: AdminUser,
    State(state): State<AppState>,
    UrlPath(id): UrlPath<i64>,
    Json(body): Json<UpdateLibraryRequest>,
) -> ApiResult<Json<LibraryDto>> {
    let name = body.name.as_deref().map(validate_name).transpose()?;
    let paths = match &body.paths {
        Some(paths) => Some(validate_paths(paths).await?),
        None => None,
    };
    if !libraries::update(&state.db, id, name.as_deref(), paths.as_deref()).await? {
        return Err(ApiError::not_found("Bibliothek nicht gefunden."));
    }
    if paths.is_some() {
        queue_scan(state.jobs.queue(), id, priority::INTERACTIVE).await?;
        resync_watcher(&state).await;
    }
    let library = libraries::get(&state.db, id)
        .await?
        .ok_or_else(|| ApiError::not_found("Bibliothek nicht gefunden."))?;
    Ok(Json(to_dto(&state, library, true).await?))
}

/// Delete a library and everything in it from Flixnet (files on disk stay untouched).
#[utoipa::path(
    delete,
    path = "/libraries/{id}",
    operation_id = "deleteLibrary",
    tag = "libraries",
    params(("id" = i64, Path, description = "Library id")),
    responses(
        (status = NO_CONTENT, description = "Library removed"),
        (status = UNAUTHORIZED, description = "Missing or invalid token", body = ErrorBody),
        (status = FORBIDDEN, description = "Not an administrator", body = ErrorBody),
        (status = NOT_FOUND, description = "No such library", body = ErrorBody),
    )
)]
async fn delete_library(
    _: AdminUser,
    State(state): State<AppState>,
    UrlPath(id): UrlPath<i64>,
) -> ApiResult<StatusCode> {
    if libraries::delete(&state.db, id).await? {
        resync_watcher(&state).await;
        Ok(StatusCode::NO_CONTENT)
    } else {
        Err(ApiError::not_found("Bibliothek nicht gefunden."))
    }
}

/// Scan a library for new, changed and removed files (administrators only).
#[utoipa::path(
    post,
    path = "/libraries/{id}/scan",
    operation_id = "scanLibrary",
    tag = "libraries",
    params(("id" = i64, Path, description = "Library id")),
    responses(
        (status = ACCEPTED, description = "Scan queued", body = ScanQueued),
        (status = UNAUTHORIZED, description = "Missing or invalid token", body = ErrorBody),
        (status = FORBIDDEN, description = "Not an administrator", body = ErrorBody),
        (status = NOT_FOUND, description = "No such library", body = ErrorBody),
    )
)]
async fn scan_library(
    _: AdminUser,
    State(state): State<AppState>,
    UrlPath(id): UrlPath<i64>,
) -> ApiResult<(StatusCode, Json<ScanQueued>)> {
    if libraries::get(&state.db, id).await?.is_none() {
        return Err(ApiError::not_found("Bibliothek nicht gefunden."));
    }
    let job_id = queue_scan(state.jobs.queue(), id, priority::INTERACTIVE).await?;
    Ok((StatusCode::ACCEPTED, Json(ScanQueued { job_id })))
}

/// Browse server folders to pick library paths (administrators only).
#[utoipa::path(
    get,
    path = "/fs/directories",
    operation_id = "listDirectories",
    tag = "libraries",
    params(DirectoriesQuery),
    responses(
        (status = OK, description = "Sub-folders of the given folder", body = DirectoryListing),
        (status = UNAUTHORIZED, description = "Missing or invalid token", body = ErrorBody),
        (status = FORBIDDEN, description = "Not an administrator", body = ErrorBody),
        (status = NOT_FOUND, description = "Folder does not exist or is not readable", body = ErrorBody),
    )
)]
async fn list_directories(
    _: AdminUser,
    Query(query): Query<DirectoriesQuery>,
) -> ApiResult<Json<DirectoryListing>> {
    let path = query.path.filter(|p| !p.trim().is_empty());
    let listing = tokio::task::spawn_blocking(move || directories(path.as_deref())).await?;
    listing.map(Json)
}

fn directories(path: Option<&str>) -> ApiResult<DirectoryListing> {
    let Some(path) = path else {
        return Ok(DirectoryListing {
            path: None,
            parent: None,
            directories: roots(),
        });
    };
    let dir = PathBuf::from(path);
    let entries = std::fs::read_dir(&dir)
        .map_err(|_| ApiError::not_found("Ordner existiert nicht oder ist nicht lesbar."))?;
    let mut directories: Vec<DirectoryEntry> = entries
        .filter_map(Result::ok)
        .filter(|e| e.file_type().is_ok_and(|t| t.is_dir() || t.is_symlink()) && e.path().is_dir())
        .filter_map(|e| {
            let name = e.file_name().to_string_lossy().into_owned();
            (!name.starts_with('.') && !name.starts_with('$')).then(|| DirectoryEntry {
                path: e.path().to_string_lossy().into_owned(),
                name,
            })
        })
        .collect();
    directories.sort_by_key(|d| d.name.to_lowercase());
    Ok(DirectoryListing {
        parent: dir.parent().map(|p| p.to_string_lossy().into_owned()),
        path: Some(dir.to_string_lossy().into_owned()),
        directories,
    })
}

#[cfg(windows)]
fn roots() -> Vec<DirectoryEntry> {
    ('A'..='Z')
        .map(|letter| format!("{letter}:\\"))
        .filter(|root| Path::new(root).exists())
        .map(|root| DirectoryEntry {
            name: root.clone(),
            path: root,
        })
        .collect()
}

#[cfg(not(windows))]
fn roots() -> Vec<DirectoryEntry> {
    vec![DirectoryEntry {
        name: "/".to_owned(),
        path: "/".to_owned(),
    }]
}

async fn visible_library(
    state: &AppState,
    user: &AuthUser,
    id: i64,
) -> ApiResult<libraries::Library> {
    let library = libraries::get(&state.db, id).await?;
    match library {
        Some(library)
            if user.is_admin || libraries::user_can_access(&state.db, user.user_id, id).await? =>
        {
            Ok(library)
        }
        _ => Err(ApiError::not_found("Bibliothek nicht gefunden.")),
    }
}

async fn to_dto(
    state: &AppState,
    library: libraries::Library,
    with_paths: bool,
) -> ApiResult<LibraryDto> {
    let counts = libraries::counts(&state.db, library.id).await?;
    let paths = if with_paths {
        Some(libraries::paths(&state.db, library.id).await?)
    } else {
        None
    };
    Ok(LibraryDto {
        id: library.id,
        name: library.name,
        kind: library.kind,
        paths,
        counts: LibraryCountsDto {
            movies: counts.movies,
            series: counts.series,
            episodes: counts.episodes,
            files: counts.files,
            unprobed: counts.unprobed,
        },
        last_scan_at: library.last_scan_at,
    })
}

fn validate_name(name: &str) -> ApiResult<String> {
    let name = name.trim();
    if name.is_empty() || name.chars().count() > 64 {
        return Err(ApiError::validation(
            "Der Name muss zwischen 1 und 64 Zeichen lang sein.",
        ));
    }
    Ok(name.to_owned())
}

/// Folders must be absolute, existing directories; duplicates are dropped.
async fn validate_paths(paths: &[String]) -> ApiResult<Vec<String>> {
    if paths.is_empty() {
        return Err(ApiError::validation("Mindestens ein Ordner ist nötig."));
    }
    let mut out: Vec<String> = Vec::new();
    for raw in paths {
        let path = raw.trim();
        if !Path::new(path).is_absolute() {
            return Err(ApiError::validation(format!(
                "„{path}“ ist kein absoluter Pfad."
            )));
        }
        let is_dir = tokio::fs::metadata(path).await.is_ok_and(|m| m.is_dir());
        if !is_dir {
            return Err(ApiError::validation(format!(
                "Der Ordner „{path}“ existiert nicht oder ist nicht lesbar."
            )));
        }
        if !out.iter().any(|p| p == path) {
            out.push(path.to_owned());
        }
    }
    Ok(out)
}

/// Point the file watcher at the current library folders.
async fn resync_watcher(state: &AppState) {
    if let Some(watcher) = &state.watcher
        && let Err(err) = watcher.sync(&state.db).await
    {
        tracing::warn!(%err, "could not update the file watcher");
    }
}
