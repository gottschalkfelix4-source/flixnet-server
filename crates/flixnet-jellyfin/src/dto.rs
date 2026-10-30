//! Jellyfin's JSON shapes (PascalCase), limited to the members the supported clients
//! read. Optional members are `Option`s and left out when empty (see [`crate::util::Jf`]).

use std::collections::BTreeMap;

use serde::Serialize;
use serde_json::Value;

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "PascalCase")]
pub struct PublicSystemInfo {
    pub local_address: Option<String>,
    pub server_name: String,
    pub version: String,
    pub product_name: String,
    pub operating_system: String,
    pub id: String,
    pub startup_wizard_completed: bool,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "PascalCase")]
pub struct SystemInfo {
    #[serde(flatten)]
    pub public: PublicSystemInfo,
    pub operating_system_display_name: String,
    pub has_pending_restart: bool,
    pub is_shutting_down: bool,
    pub supports_library_monitor: bool,
    pub web_socket_port_number: u16,
    pub completed_installations: Vec<Value>,
    pub can_self_restart: bool,
    pub can_launch_web_browser: bool,
    pub has_update_available: bool,
    pub cast_receiver_applications: Vec<Value>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "PascalCase")]
pub struct UserDto {
    pub name: String,
    pub server_id: String,
    pub server_name: String,
    pub id: String,
    pub has_password: bool,
    pub has_configured_password: bool,
    pub has_configured_easy_password: bool,
    pub enable_auto_login: bool,
    pub last_login_date: Option<String>,
    pub last_activity_date: Option<String>,
    pub configuration: UserConfiguration,
    pub policy: UserPolicy,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "PascalCase")]
pub struct UserConfiguration {
    pub audio_language_preference: Option<String>,
    pub play_default_audio_track: bool,
    pub subtitle_language_preference: Option<String>,
    pub display_missing_episodes: bool,
    pub grouped_folders: Vec<String>,
    pub subtitle_mode: &'static str,
    pub display_collections_view: bool,
    pub enable_local_password: bool,
    pub ordered_views: Vec<String>,
    pub latest_items_excludes: Vec<String>,
    pub my_media_excludes: Vec<String>,
    pub hide_played_in_latest: bool,
    pub remember_audio_selections: bool,
    pub remember_subtitle_selections: bool,
    pub enable_next_episode_auto_play: bool,
}

impl Default for UserConfiguration {
    fn default() -> Self {
        Self {
            audio_language_preference: None,
            play_default_audio_track: true,
            subtitle_language_preference: None,
            display_missing_episodes: false,
            grouped_folders: Vec::new(),
            subtitle_mode: "Default",
            display_collections_view: false,
            enable_local_password: false,
            ordered_views: Vec::new(),
            latest_items_excludes: Vec::new(),
            my_media_excludes: Vec::new(),
            hide_played_in_latest: true,
            remember_audio_selections: true,
            remember_subtitle_selections: true,
            enable_next_episode_auto_play: true,
        }
    }
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "PascalCase")]
pub struct UserPolicy {
    pub is_administrator: bool,
    pub is_hidden: bool,
    pub enable_collection_management: bool,
    pub enable_subtitle_management: bool,
    pub enable_lyric_management: bool,
    pub is_disabled: bool,
    pub max_parental_rating: Option<i64>,
    pub blocked_tags: Vec<String>,
    pub allowed_tags: Vec<String>,
    pub enable_user_preference_access: bool,
    pub access_schedules: Vec<Value>,
    pub block_unrated_items: Vec<String>,
    pub enable_remote_control_of_other_users: bool,
    pub enable_shared_device_control: bool,
    pub enable_remote_access: bool,
    pub enable_live_tv_management: bool,
    pub enable_live_tv_access: bool,
    pub enable_media_playback: bool,
    pub enable_audio_playback_transcoding: bool,
    pub enable_video_playback_transcoding: bool,
    pub enable_playback_remuxing: bool,
    pub force_remote_source_transcoding: bool,
    pub enable_content_deletion: bool,
    pub enable_content_deletion_from_folders: Vec<String>,
    pub enable_content_downloading: bool,
    pub enable_sync_transcoding: bool,
    pub enable_media_conversion: bool,
    pub enabled_devices: Vec<String>,
    pub enable_all_devices: bool,
    pub enabled_channels: Vec<String>,
    pub enable_all_channels: bool,
    pub enabled_folders: Vec<String>,
    pub enable_all_folders: bool,
    pub invalid_login_attempt_count: i64,
    pub login_attempts_before_lockout: i64,
    pub max_active_sessions: i64,
    pub enable_public_sharing: bool,
    pub blocked_media_folders: Vec<String>,
    pub blocked_channels: Vec<String>,
    pub remote_client_bitrate_limit: i64,
    pub authentication_provider_id: &'static str,
    pub password_reset_provider_id: &'static str,
    pub sync_play_access: &'static str,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "PascalCase")]
pub struct AuthenticationResult {
    pub user: UserDto,
    pub session_info: SessionInfoDto,
    pub access_token: String,
    pub server_id: String,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "PascalCase")]
pub struct PlayerStateInfo {
    pub position_ticks: Option<i64>,
    pub can_seek: bool,
    pub is_paused: bool,
    pub is_muted: bool,
    pub media_source_id: Option<String>,
    pub play_method: Option<&'static str>,
    pub repeat_mode: &'static str,
    pub playback_order: &'static str,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "PascalCase")]
pub struct SessionInfoDto {
    pub play_state: PlayerStateInfo,
    pub additional_users: Vec<Value>,
    pub playable_media_types: Vec<&'static str>,
    pub id: String,
    pub user_id: String,
    pub user_name: String,
    pub client: Option<String>,
    pub last_activity_date: String,
    pub last_playback_check_in: String,
    pub device_name: Option<String>,
    pub device_id: Option<String>,
    pub application_version: Option<String>,
    pub now_playing_item: Option<Box<BaseItemDto>>,
    pub is_active: bool,
    pub supports_media_control: bool,
    pub supports_remote_control: bool,
    pub now_playing_queue: Vec<Value>,
    pub has_custom_device_name: bool,
    pub server_id: String,
    pub supported_commands: Vec<&'static str>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "PascalCase")]
pub struct QueryResult<T> {
    pub items: Vec<T>,
    pub total_record_count: i64,
    pub start_index: i64,
}

#[derive(Debug, Clone, Default, Serialize)]
#[serde(rename_all = "PascalCase")]
pub struct UserItemDataDto {
    pub rating: Option<f64>,
    pub played_percentage: Option<f64>,
    pub unplayed_item_count: Option<i64>,
    pub playback_position_ticks: i64,
    pub play_count: i64,
    pub is_favorite: bool,
    pub last_played_date: Option<String>,
    pub played: bool,
    pub key: String,
    pub item_id: String,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "PascalCase")]
pub struct NameGuidPair {
    pub name: String,
    pub id: String,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "PascalCase")]
pub struct BaseItemPerson {
    pub name: String,
    pub id: String,
    pub role: Option<String>,
    #[serde(rename = "Type")]
    pub kind: &'static str,
    pub primary_image_tag: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "PascalCase")]
pub struct ChapterInfo {
    pub start_position_ticks: i64,
    pub name: Option<String>,
    pub image_date_modified: String,
}

/// Movies, series, seasons, episodes, views, people and genres alike.
#[derive(Debug, Clone, Default, Serialize)]
#[serde(rename_all = "PascalCase")]
pub struct BaseItemDto {
    pub name: String,
    pub original_title: Option<String>,
    pub server_id: String,
    pub id: String,
    pub etag: Option<String>,
    pub date_created: Option<String>,
    pub can_delete: Option<bool>,
    pub can_download: Option<bool>,
    pub sort_name: Option<String>,
    pub premiere_date: Option<String>,
    pub external_urls: Option<Vec<Value>>,
    pub media_sources: Option<Vec<MediaSourceInfo>>,
    pub path: Option<String>,
    pub official_rating: Option<String>,
    pub overview: Option<String>,
    pub taglines: Option<Vec<String>>,
    pub genres: Option<Vec<String>>,
    pub community_rating: Option<f64>,
    pub run_time_ticks: Option<i64>,
    pub production_year: Option<i64>,
    pub index_number: Option<i64>,
    pub index_number_end: Option<i64>,
    pub parent_index_number: Option<i64>,
    pub remote_trailers: Option<Vec<Value>>,
    pub provider_ids: Option<BTreeMap<String, String>>,
    pub is_folder: bool,
    pub parent_id: Option<String>,
    #[serde(rename = "Type")]
    pub kind: &'static str,
    pub people: Option<Vec<BaseItemPerson>>,
    pub studios: Option<Vec<NameGuidPair>>,
    pub genre_items: Option<Vec<NameGuidPair>>,
    pub parent_logo_item_id: Option<String>,
    pub parent_logo_image_tag: Option<String>,
    pub parent_backdrop_item_id: Option<String>,
    pub parent_backdrop_image_tags: Option<Vec<String>>,
    pub local_trailer_count: Option<i64>,
    pub user_data: Option<UserItemDataDto>,
    pub recursive_item_count: Option<i64>,
    pub child_count: Option<i64>,
    pub series_name: Option<String>,
    pub series_id: Option<String>,
    pub season_id: Option<String>,
    pub special_feature_count: Option<i64>,
    pub display_preferences_id: Option<String>,
    pub primary_image_aspect_ratio: Option<f64>,
    pub collection_type: Option<&'static str>,
    pub series_primary_image_tag: Option<String>,
    pub season_name: Option<String>,
    pub media_streams: Option<Vec<MediaStream>>,
    pub video_type: Option<&'static str>,
    pub part_count: Option<i64>,
    pub media_source_count: Option<i64>,
    pub image_tags: BTreeMap<String, String>,
    pub backdrop_image_tags: Vec<String>,
    pub image_blur_hashes: Option<BTreeMap<String, BTreeMap<String, String>>>,
    pub parent_thumb_item_id: Option<String>,
    pub parent_thumb_image_tag: Option<String>,
    pub chapters: Option<Vec<ChapterInfo>>,
    pub location_type: Option<&'static str>,
    pub media_type: &'static str,
    pub width: Option<i64>,
    pub height: Option<i64>,
    pub container: Option<String>,
    pub has_subtitles: Option<bool>,
    pub movie_count: Option<i64>,
    pub series_count: Option<i64>,
    pub episode_count: Option<i64>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "PascalCase")]
pub struct MediaSourceInfo {
    pub protocol: &'static str,
    pub id: String,
    pub path: Option<String>,
    #[serde(rename = "Type")]
    pub kind: &'static str,
    pub container: Option<String>,
    pub size: Option<i64>,
    pub name: Option<String>,
    pub is_remote: bool,
    #[serde(rename = "ETag")]
    pub etag: Option<String>,
    pub run_time_ticks: Option<i64>,
    pub read_at_native_framerate: bool,
    pub ignore_dts: bool,
    pub ignore_index: bool,
    pub gen_pts_input: bool,
    pub supports_transcoding: bool,
    pub supports_direct_stream: bool,
    pub supports_direct_play: bool,
    pub is_infinite_stream: bool,
    pub use_most_compatible_transcoding_profile: bool,
    pub requires_opening: bool,
    pub requires_closing: bool,
    pub requires_looping: bool,
    pub supports_probing: bool,
    pub video_type: Option<&'static str>,
    pub media_streams: Vec<MediaStream>,
    pub media_attachments: Vec<Value>,
    pub formats: Vec<String>,
    pub bitrate: Option<i64>,
    pub required_http_headers: BTreeMap<String, String>,
    pub transcoding_url: Option<String>,
    pub transcoding_sub_protocol: &'static str,
    pub transcoding_container: Option<&'static str>,
    pub default_audio_stream_index: Option<i64>,
    pub default_subtitle_stream_index: Option<i64>,
    pub has_segments: bool,
}

#[derive(Debug, Clone, Default, Serialize)]
#[serde(rename_all = "PascalCase")]
pub struct MediaStream {
    pub codec: Option<String>,
    pub language: Option<String>,
    pub color_transfer: Option<String>,
    pub color_primaries: Option<String>,
    pub title: Option<String>,
    pub video_range: &'static str,
    pub video_range_type: &'static str,
    pub audio_spatial_format: &'static str,
    pub display_title: Option<String>,
    pub is_interlaced: bool,
    pub channel_layout: Option<String>,
    pub bit_rate: Option<i64>,
    pub bit_depth: Option<i64>,
    pub channels: Option<i64>,
    pub sample_rate: Option<i64>,
    pub is_default: bool,
    pub is_forced: bool,
    pub is_hearing_impaired: bool,
    pub is_original: bool,
    pub height: Option<i64>,
    pub width: Option<i64>,
    pub average_frame_rate: Option<f64>,
    pub real_frame_rate: Option<f64>,
    pub profile: Option<String>,
    #[serde(rename = "Type")]
    pub kind: &'static str,
    pub aspect_ratio: Option<String>,
    pub index: i64,
    pub is_external: bool,
    pub delivery_method: Option<&'static str>,
    pub delivery_url: Option<String>,
    pub is_external_url: Option<bool>,
    pub is_text_subtitle_stream: bool,
    pub supports_external_stream: bool,
    pub pixel_format: Option<String>,
    pub level: Option<f64>,
}

/// An intro, outro or recap (`MediaSegments/{id}`, Jellyfin 10.10+).
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "PascalCase")]
pub struct MediaSegmentDto {
    pub id: String,
    pub item_id: String,
    #[serde(rename = "Type")]
    pub kind: &'static str,
    pub start_ticks: i64,
    pub end_ticks: i64,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "PascalCase")]
pub struct PlaybackInfoResponse {
    pub media_sources: Vec<MediaSourceInfo>,
    pub play_session_id: String,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "PascalCase")]
pub struct SearchHint {
    pub item_id: String,
    pub id: String,
    pub name: String,
    pub matched_term: Option<String>,
    pub index_number: Option<i64>,
    pub production_year: Option<i64>,
    pub parent_index_number: Option<i64>,
    pub primary_image_tag: Option<String>,
    pub backdrop_image_tag: Option<String>,
    pub backdrop_image_item_id: Option<String>,
    #[serde(rename = "Type")]
    pub kind: &'static str,
    pub is_folder: bool,
    pub run_time_ticks: Option<i64>,
    pub media_type: &'static str,
    pub series: Option<String>,
    pub artists: Vec<String>,
    pub primary_image_aspect_ratio: Option<f64>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "PascalCase")]
pub struct SearchHintResult {
    pub search_hints: Vec<SearchHint>,
    pub total_record_count: i64,
}
