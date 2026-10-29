//! Input validation shared by the account endpoints.

use flixnet_core::auth;
use serde::{Deserialize, Deserializer};

use crate::error::{ApiError, ApiResult};

pub fn name(name: &str) -> ApiResult<String> {
    let name = name.trim();
    if name.is_empty() || name.chars().count() > 64 {
        return Err(ApiError::validation(
            "Der Name muss zwischen 1 und 64 Zeichen lang sein.",
        ));
    }
    if name.chars().any(char::is_control) {
        return Err(ApiError::validation("Der Name enthält ungültige Zeichen."));
    }
    Ok(name.to_owned())
}

pub fn password(password: &str) -> ApiResult<()> {
    let len = password.chars().count();
    if !(auth::PASSWORD_MIN_LEN..=auth::PASSWORD_MAX_LEN).contains(&len) {
        return Err(ApiError::validation(format!(
            "Das Passwort muss mindestens {} Zeichen lang sein.",
            auth::PASSWORD_MIN_LEN
        )));
    }
    Ok(())
}

/// PINs are 4–8 digits.
pub fn pin(pin: &str) -> ApiResult<()> {
    if (4..=8).contains(&pin.len()) && pin.bytes().all(|b| b.is_ascii_digit()) {
        Ok(())
    } else {
        Err(ApiError::validation(
            "Die PIN muss aus 4 bis 8 Ziffern bestehen.",
        ))
    }
}

/// Age limit for content (FSK/MPAA ratings are mapped to ages): 0–21.
pub fn age_rating(age: Option<i64>) -> ApiResult<()> {
    match age {
        Some(age) if !(0..=21).contains(&age) => Err(ApiError::validation(
            "Die Altersfreigabe muss zwischen 0 und 21 liegen.",
        )),
        _ => Ok(()),
    }
}

/// Blocked genres of a profile, as stored: a JSON array (trimmed, without duplicates),
/// `None` for an empty list.
pub fn genre_list(genres: &[String]) -> ApiResult<Option<String>> {
    let mut list: Vec<String> = Vec::new();
    for genre in genres {
        let genre = genre.trim();
        if genre.is_empty() {
            continue;
        }
        if genre.chars().count() > 100 || genre.chars().any(char::is_control) {
            return Err(ApiError::validation("Ungültiger Genre-Name."));
        }
        if !list.iter().any(|g| g.eq_ignore_ascii_case(genre)) {
            list.push(genre.to_owned());
        }
    }
    if list.len() > 100 {
        return Err(ApiError::validation("Höchstens 100 Genres sperren."));
    }
    if list.is_empty() {
        return Ok(None);
    }
    list.sort();
    Ok(Some(
        serde_json::to_string(&list).map_err(ApiError::internal)?,
    ))
}

/// Distinguishes an absent field (`None`) from an explicit `null` (`Some(None)`).
pub fn double_option<'de, T, D>(deserializer: D) -> Result<Option<Option<T>>, D::Error>
where
    T: Deserialize<'de>,
    D: Deserializer<'de>,
{
    Option::<T>::deserialize(deserializer).map(Some)
}
