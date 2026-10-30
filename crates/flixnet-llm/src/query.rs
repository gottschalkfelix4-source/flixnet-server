//! The filter DSL the LLM fills in for natural-language search ("lustige Filme aus
//! den 90ern unter 2 Stunden, die ich noch nicht gesehen habe"), its JSON schema and
//! a lenient, validating parser. The caller maps it onto the browse `ItemFilter`
//! (with the viewer's restrictions), so the LLM can only narrow what the viewer may
//! see anyway.

use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::client::{
    ChatRequest, LlmClient, LlmError, Message, ToolChoice, ToolSpec, extract_json_object,
    parse_arguments,
};
use crate::prompts;

pub const FILTER_TOOL: &str = "set_filter";
pub const SORTS: &[&str] = &["relevance", "title", "year", "rating", "date_added"];
pub const MAX_LIMIT: i64 = 50;

/// What the model may set. Everything optional; unknown values are dropped.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct CatalogQuery {
    /// `movie`, `series`, `episode`.
    pub kinds: Vec<String>,
    /// Exact genre names of the catalogue.
    pub genres: Vec<String>,
    pub year_from: Option<i64>,
    pub year_to: Option<i64>,
    /// Community rating 0–10.
    pub min_rating: Option<f64>,
    pub min_runtime_minutes: Option<i64>,
    pub max_runtime_minutes: Option<i64>,
    /// `false` = not watched yet.
    pub played: Option<bool>,
    pub favorite: Option<bool>,
    /// Names of actors, directors, …
    pub people: Vec<String>,
    /// Remaining keywords for the full-text search.
    pub search_text: Option<String>,
    pub sort: Option<String>,
    pub descending: Option<bool>,
    pub limit: Option<i64>,
}

impl CatalogQuery {
    pub fn is_empty(&self) -> bool {
        *self == CatalogQuery::default()
    }
}

/// JSON schema of [`CatalogQuery`]; `genres` is limited to the catalogue's genres
/// (only those the viewer may see).
pub fn schema(genres: &[String]) -> Value {
    let genre_items = if genres.is_empty() {
        json!({ "type": "string" })
    } else {
        json!({ "type": "string", "enum": genres })
    };
    json!({
        "type": "object",
        "additionalProperties": false,
        "properties": {
            "kinds": { "type": "array", "items": { "type": "string", "enum": ["movie", "series", "episode"] } },
            "genres": { "type": "array", "items": genre_items },
            "year_from": { "type": "integer", "minimum": 1870, "maximum": 2100 },
            "year_to": { "type": "integer", "minimum": 1870, "maximum": 2100 },
            "min_rating": { "type": "number", "minimum": 0, "maximum": 10 },
            "min_runtime_minutes": { "type": "integer", "minimum": 1 },
            "max_runtime_minutes": { "type": "integer", "minimum": 1 },
            "played": { "type": "boolean", "description": "false = noch nicht gesehen" },
            "favorite": { "type": "boolean" },
            "people": { "type": "array", "items": { "type": "string" }, "description": "Namen von Schauspielern oder Regie" },
            "search_text": { "type": "string", "description": "Stichwörter für die Volltextsuche" },
            "sort": { "type": "string", "enum": SORTS },
            "descending": { "type": "boolean" },
            "limit": { "type": "integer", "minimum": 1, "maximum": MAX_LIMIT }
        }
    })
}

pub fn filter_tool(genres: &[String]) -> ToolSpec {
    ToolSpec {
        name: FILTER_TOOL.to_owned(),
        description: "Setzt den Filter für die Katalogsuche.".to_owned(),
        parameters: schema(genres),
    }
}

fn int(v: &Value) -> Option<i64> {
    match v {
        Value::Number(n) => n.as_i64().or_else(|| n.as_f64().map(|f| f.round() as i64)),
        Value::String(s) => s.trim().parse().ok(),
        _ => None,
    }
}

fn float(v: &Value) -> Option<f64> {
    match v {
        Value::Number(n) => n.as_f64(),
        Value::String(s) => s.trim().replace(',', ".").parse().ok(),
        _ => None,
    }
    .filter(|f: &f64| f.is_finite())
}

fn boolean(v: &Value) -> Option<bool> {
    match v {
        Value::Bool(b) => Some(*b),
        Value::String(s) => match s.trim().to_lowercase().as_str() {
            "true" | "ja" | "yes" => Some(true),
            "false" | "nein" | "no" => Some(false),
            _ => None,
        },
        _ => None,
    }
}

fn strings(v: &Value) -> Vec<String> {
    match v {
        Value::Array(items) => items
            .iter()
            .filter_map(|i| i.as_str())
            .map(|s| s.trim().to_owned())
            .filter(|s| !s.is_empty())
            .collect(),
        Value::String(s) if !s.trim().is_empty() => s
            .split(',')
            .map(|p| p.trim().to_owned())
            .filter(|p| !p.is_empty())
            .collect(),
        _ => Vec::new(),
    }
}

fn kind(value: &str) -> Option<&'static str> {
    match value.to_lowercase().as_str() {
        "movie" | "movies" | "film" | "filme" => Some("movie"),
        "series" | "serie" | "serien" | "show" | "shows" | "tv" => Some("series"),
        "episode" | "episodes" | "folge" | "folgen" => Some("episode"),
        _ => None,
    }
}

/// Validates tool-call arguments into a query: numbers given as strings are accepted,
/// out-of-range values are clamped or dropped, genres must be in `genres` (case
/// does not matter), at most [`MAX_LIMIT`] results. `Err` only when `args` is not
/// an object.
pub fn parse_query(args: &Value, genres: &[String]) -> Result<CatalogQuery, String> {
    let Some(obj) = args.as_object() else {
        return Err("arguments are not a JSON object".to_owned());
    };
    let get = |key: &str| obj.get(key).unwrap_or(&Value::Null);
    let year = |key: &str| int(get(key)).filter(|y| (1870..=2100).contains(y));
    let mut q = CatalogQuery {
        kinds: strings(get("kinds"))
            .iter()
            .filter_map(|k| kind(k))
            .map(str::to_owned)
            .collect(),
        genres: strings(get("genres"))
            .iter()
            .filter_map(|g| genres.iter().find(|known| known.eq_ignore_ascii_case(g)))
            .cloned()
            .collect(),
        year_from: year("year_from"),
        year_to: year("year_to"),
        min_rating: float(get("min_rating")).map(|r| r.clamp(0.0, 10.0)),
        min_runtime_minutes: int(get("min_runtime_minutes")).filter(|m| (1..=1000).contains(m)),
        max_runtime_minutes: int(get("max_runtime_minutes")).filter(|m| (1..=1000).contains(m)),
        played: boolean(get("played")),
        favorite: boolean(get("favorite")).filter(|f| *f),
        people: strings(get("people")).into_iter().take(5).collect(),
        search_text: get("search_text")
            .as_str()
            .map(|s| s.trim().chars().take(120).collect::<String>())
            .filter(|s| !s.is_empty()),
        sort: get("sort")
            .as_str()
            .map(str::to_lowercase)
            .filter(|s| SORTS.contains(&s.as_str())),
        descending: boolean(get("descending")),
        limit: int(get("limit")).map(|l| l.clamp(1, MAX_LIMIT)),
    };
    q.kinds.dedup();
    q.genres.dedup();
    if let (Some(from), Some(to)) = (q.year_from, q.year_to)
        && from > to
    {
        (q.year_from, q.year_to) = (Some(to), Some(from));
    }
    if let (Some(min), Some(max)) = (q.min_runtime_minutes, q.max_runtime_minutes)
        && min > max
    {
        (q.min_runtime_minutes, q.max_runtime_minutes) = (Some(max), Some(min));
    }
    Ok(q)
}

#[derive(Debug, thiserror::Error)]
pub enum NaturalSearchError {
    #[error(transparent)]
    Llm(#[from] LlmError),
    #[error("the model did not produce a usable filter: {0}")]
    Malformed(String),
}

/// Arguments of a `set_filter` call in a reply: a proper tool call, or JSON in the
/// text (`{"name": "set_filter", "arguments": {…}}` or the arguments themselves).
fn filter_arguments(reply: &Message) -> Result<Value, String> {
    if let Some(call) = reply.tool_calls.first() {
        if call.function.name != FILTER_TOOL {
            return Err(format!("unexpected function {}", call.function.name));
        }
        return parse_arguments(&call.function.arguments)
            .ok_or_else(|| format!("invalid arguments: {}", call.function.arguments));
    }
    let text = reply.content.as_deref().unwrap_or_default();
    let value = extract_json_object(text).ok_or_else(|| "no tool call and no JSON".to_owned())?;
    match value.get("arguments").or_else(|| value.get("parameters")) {
        Some(args) if value.get("name").is_some() => match args {
            Value::String(s) => parse_arguments(s).ok_or_else(|| "invalid arguments".to_owned()),
            other => Ok(other.clone()),
        },
        _ => Ok(value),
    }
}

/// Asks the model for a filter. Malformed output gets one corrective retry; after
/// that the caller falls back to keyword search.
pub async fn natural_query(
    client: &LlmClient,
    text: &str,
    genres: &[String],
    current_year: i32,
) -> Result<CatalogQuery, NaturalSearchError> {
    let mut genre_note = String::from("Erlaubte Genres: ");
    genre_note.push_str(&genres.join(", "));
    let mut request = ChatRequest::new(vec![
        Message::system(format!(
            "{}\n{genre_note}",
            prompts::natural_search(current_year)
        )),
        Message::user(text.chars().take(500).collect::<String>()),
    ]);
    request.tools = vec![filter_tool(genres)];
    request.tool_choice = ToolChoice::Function(FILTER_TOOL.to_owned());
    request.temperature = 0.0;
    let mut last_error = String::new();
    for attempt in 0..2 {
        let reply = client.chat(&request).await?;
        match filter_arguments(&reply).and_then(|args| parse_query(&args, genres)) {
            Ok(query) => return Ok(query),
            Err(err) => {
                tracing::debug!(attempt, %err, "LLM filter output malformed");
                last_error = err;
                request.messages.push(Message::assistant(
                    reply
                        .content
                        .clone()
                        .unwrap_or_else(|| "(ungültiger Funktionsaufruf)".to_owned()),
                ));
                request.messages.push(Message::user(
                    "Das war kein gültiger Aufruf. Rufe set_filter mit einem gültigen JSON-Objekt auf.",
                ));
            }
        }
    }
    Err(NaturalSearchError::Malformed(last_error))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn genres() -> Vec<String> {
        vec!["Komödie".into(), "Horror".into(), "Drama".into()]
    }

    #[test]
    fn lenient_values_are_validated() {
        let q = parse_query(
            &json!({
                "kinds": ["Filme", "unbekannt"],
                "genres": ["komödie", "Western"],
                "year_from": "1999", "year_to": 1990,
                "min_rating": "7,5",
                "max_runtime_minutes": 120.4,
                "min_runtime_minutes": -3,
                "played": "nein",
                "favorite": false,
                "people": "Tom Hanks, Meg Ryan",
                "search_text": "  Strand ",
                "sort": "RATING",
                "limit": 500,
                "extra": 1
            }),
            &genres(),
        )
        .unwrap();
        assert_eq!(q.kinds, vec!["movie"]);
        assert_eq!(q.genres, vec!["Komödie"]);
        assert_eq!((q.year_from, q.year_to), (Some(1990), Some(1999)));
        assert_eq!(q.min_rating, Some(7.5));
        assert_eq!(q.max_runtime_minutes, Some(120));
        assert_eq!(q.min_runtime_minutes, None);
        assert_eq!(q.played, Some(false));
        assert_eq!(q.favorite, None);
        assert_eq!(q.people, vec!["Tom Hanks", "Meg Ryan"]);
        assert_eq!(q.search_text.as_deref(), Some("Strand"));
        assert_eq!(q.sort.as_deref(), Some("rating"));
        assert_eq!(q.limit, Some(MAX_LIMIT));
        assert!(parse_query(&json!([1]), &genres()).is_err());
        assert!(parse_query(&json!({}), &genres()).unwrap().is_empty());
    }

    #[test]
    fn the_schema_only_offers_visible_genres() {
        let s = schema(&genres());
        assert_eq!(s["properties"]["genres"]["items"]["enum"], json!(genres()));
        assert_eq!(s["additionalProperties"], json!(false));
    }

    #[test]
    fn filter_json_is_taken_from_text_answers() {
        let reply = Message::assistant(
            "Hier: {\"name\": \"set_filter\", \"arguments\": {\"kinds\": [\"movie\"]}}",
        );
        assert_eq!(
            filter_arguments(&reply).unwrap(),
            json!({"kinds": ["movie"]})
        );
        let reply = Message::assistant("```json\n{\"played\": false}\n```");
        assert_eq!(filter_arguments(&reply).unwrap(), json!({"played": false}));
        assert!(filter_arguments(&Message::assistant("Keine Ahnung.")).is_err());
    }
}
