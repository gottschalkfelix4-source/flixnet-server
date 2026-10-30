//! "Was bisher geschah": spoiler-free recaps. The caller hands in only material from
//! before the viewer's position — earlier episodes (overview and dialogue) and the
//! current one up to the position — and this module never adds anything. Dialogue
//! is trimmed to a budget from the most recent end, so the recap concentrates on
//! what happened last.

use crate::client::{ChatRequest, LlmClient, LlmError, Message, strip_thinking};
use crate::prompts;

/// Dialogue characters sent per recap (about 4–5k tokens).
pub const DIALOGUE_BUDGET: usize = 16_000;

/// One earlier episode, or the current one up to the position.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct RecapPart {
    /// "Staffel 1, Folge 2: Titel" or the movie title.
    pub label: String,
    /// Overview of a finished part (never of the current one: it may tell the end).
    pub overview: Option<String>,
    /// Dialogue chunks in time order: (start in ms, text).
    pub dialogue: Vec<(i64, String)>,
}

fn clock(ms: i64) -> String {
    let s = ms / 1000;
    format!("{}:{:02}:{:02}", s / 3600, s / 60 % 60, s % 60)
}

/// The user message: parts in order, dialogue cut to `budget` characters taken from
/// the end (the newest dialogue survives).
pub fn material(title: &str, parts: &[RecapPart], budget: usize) -> String {
    let mut remaining = budget;
    // Walk backwards to decide which dialogue lines fit.
    let mut kept: Vec<Vec<(i64, &str)>> = vec![Vec::new(); parts.len()];
    for (index, part) in parts.iter().enumerate().rev() {
        for (start, text) in part.dialogue.iter().rev() {
            let cost = text.len() + 12;
            if cost > remaining {
                remaining = 0;
                break;
            }
            remaining -= cost;
            kept[index].push((*start, text.as_str()));
        }
        kept[index].reverse();
    }
    let mut out = format!("Titel: {title}\n");
    for (part, dialogue) in parts.iter().zip(&kept) {
        out.push_str(&format!("\n## {}\n", part.label));
        if let Some(overview) = &part.overview {
            out.push_str(&format!("Inhalt: {}\n", overview.trim()));
        }
        if !dialogue.is_empty() {
            out.push_str("Dialog:\n");
            for (start, text) in dialogue {
                out.push_str(&format!("[{}] {}\n", clock(*start), text));
            }
        }
    }
    out
}

/// Asks the model for the recap.
pub async fn generate(
    client: &LlmClient,
    title: &str,
    parts: &[RecapPart],
) -> Result<String, LlmError> {
    let mut request = ChatRequest::new(vec![
        Message::system(prompts::RECAP),
        Message::user(material(title, parts, DIALOGUE_BUDGET)),
    ]);
    request.max_tokens = Some(600);
    let reply = client.chat(&request).await?;
    let text = strip_thinking(reply.content.as_deref().unwrap_or_default());
    if text.is_empty() {
        return Err(LlmError::Malformed("empty recap".to_owned()));
    }
    Ok(text)
}

/// Without an LLM: the overviews of the last finished parts.
pub fn fallback(parts: &[RecapPart]) -> Option<String> {
    let lines: Vec<String> = parts
        .iter()
        .filter_map(|p| {
            p.overview
                .as_ref()
                .map(|o| format!("{}: {}", p.label, o.trim()))
        })
        .collect();
    if lines.is_empty() {
        return None;
    }
    let start = lines.len().saturating_sub(3);
    Some(lines[start..].join("\n\n"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_newest_dialogue_survives_the_budget() {
        let parts = vec![
            RecapPart {
                label: "Folge 1".into(),
                overview: Some("Es beginnt.".into()),
                dialogue: vec![(0, "alt ".repeat(20)), (60_000, "älter".into())],
            },
            RecapPart {
                label: "Folge 2 (bis 0:01:00)".into(),
                overview: None,
                dialogue: vec![(1_000, "neu eins".into()), (30_000, "neu zwei".into())],
            },
        ];
        let text = material("Serie", &parts, 60);
        assert!(text.contains("Inhalt: Es beginnt."));
        assert!(text.contains("[0:00:01] neu eins") && text.contains("[0:00:30] neu zwei"));
        assert!(!text.contains("alt alt"), "{text}");
        assert!(text.find("Folge 1").unwrap() < text.find("Folge 2").unwrap());
    }

    #[test]
    fn the_fallback_uses_the_last_overviews() {
        let part = |n: i64, overview: Option<&str>| RecapPart {
            label: format!("Folge {n}"),
            overview: overview.map(str::to_owned),
            dialogue: Vec::new(),
        };
        let parts = vec![
            part(1, Some("A")),
            part(2, Some("B")),
            part(3, Some("C")),
            part(4, Some("D")),
            part(5, None),
        ];
        assert_eq!(
            fallback(&parts).unwrap(),
            "Folge 2: B\n\nFolge 3: C\n\nFolge 4: D"
        );
        assert_eq!(fallback(&[part(1, None)]), None);
    }
}
