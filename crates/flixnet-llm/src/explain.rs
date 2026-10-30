//! "Weil du X und Y mochtest …": why a title fits the viewer, from their own
//! history only. The caller collects the history entries (visible items only) and
//! what they share with the target; the LLM merely phrases it. Its answer is checked
//! — every quoted title must come from the history — otherwise the rule-based
//! sentence is used, which also serves when no LLM is configured.

use crate::client::{ChatRequest, LlmClient, Message, strip_thinking};
use crate::prompts;

/// The title being explained.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Target {
    pub title: String,
    pub year: Option<i64>,
    pub kind: String,
    pub genres: Vec<String>,
    pub overview: Option<String>,
}

/// A title of the viewer's history and what it shares with the target.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct HistoryEntry {
    pub title: String,
    pub year: Option<i64>,
    pub favorite: bool,
    pub shared_genres: Vec<String>,
    pub shared_people: Vec<String>,
    pub same_collection: bool,
}

impl HistoryEntry {
    /// How strongly it relates to the target (people and collections count most).
    pub fn weight(&self) -> usize {
        self.shared_genres.len()
            + 2 * self.shared_people.len()
            + if self.same_collection { 3 } else { 0 }
            + usize::from(self.favorite)
    }
}

fn quoted(title: &str) -> String {
    format!("„{title}“")
}

fn list(items: &[String]) -> String {
    match items {
        [] => String::new(),
        [one] => one.clone(),
        [init @ .., last] => format!("{} und {last}", init.join(", ")),
    }
}

/// The sentence without an LLM, from the strongest one or two related titles.
pub fn rule_based(target: &Target, history: &[HistoryEntry]) -> Option<String> {
    let mut related: Vec<&HistoryEntry> = history.iter().filter(|h| h.weight() > 0).collect();
    related.sort_by_key(|h| std::cmp::Reverse(h.weight()));
    let top: Vec<&HistoryEntry> = related.into_iter().take(2).collect();
    if top.is_empty() {
        return None;
    }
    let titles = list(&top.iter().map(|h| quoted(&h.title)).collect::<Vec<_>>());
    let verb = if top.iter().all(|h| h.favorite) {
        "mochtest"
    } else {
        "gesehen hast"
    };
    let mut reasons = Vec::new();
    let people: Vec<String> = top
        .iter()
        .flat_map(|h| h.shared_people.iter().cloned())
        .fold(Vec::new(), |mut acc, p| {
            if !acc.contains(&p) {
                acc.push(p);
            }
            acc
        });
    if !people.is_empty() {
        reasons.push(format!(
            "ebenfalls mit {}",
            list(&people[..people.len().min(2)])
        ));
    }
    if top.iter().any(|h| h.same_collection) {
        reasons.push("aus derselben Reihe".to_owned());
    }
    let genres: Vec<String> = target
        .genres
        .iter()
        .filter(|g| top.iter().any(|h| h.shared_genres.contains(g)))
        .take(2)
        .cloned()
        .collect();
    if !genres.is_empty() {
        reasons.push(format!("auch {}", list(&genres)));
    }
    let tail = if reasons.is_empty() {
        String::new()
    } else {
        format!(" – {}", reasons.join(", "))
    };
    Some(format!("Weil du {titles} {verb}{tail}."))
}

fn describe(target: &Target, history: &[HistoryEntry]) -> String {
    let mut out = format!(
        "Titel: {}{} ({})\nGenres: {}\n",
        target.title,
        target.year.map(|y| format!(" ({y})")).unwrap_or_default(),
        target.kind,
        target.genres.join(", ")
    );
    if let Some(overview) = &target.overview {
        out.push_str("Beschreibung: ");
        out.extend(overview.chars().take(400));
        out.push('\n');
    }
    out.push_str("\nVerlauf des Nutzers (nur diese Titel dürfen genannt werden):\n");
    for h in history {
        let mut shared = Vec::new();
        if !h.shared_genres.is_empty() {
            shared.push(format!("gleiche Genres: {}", h.shared_genres.join(", ")));
        }
        if !h.shared_people.is_empty() {
            shared.push(format!("gleiche Personen: {}", h.shared_people.join(", ")));
        }
        if h.same_collection {
            shared.push("gleiche Reihe".to_owned());
        }
        out.push_str(&format!(
            "- {}{}{}: {}\n",
            h.title,
            h.year.map(|y| format!(" ({y})")).unwrap_or_default(),
            if h.favorite { ", Favorit" } else { "" },
            if shared.is_empty() {
                "keine direkte Gemeinsamkeit".to_owned()
            } else {
                shared.join("; ")
            }
        ));
    }
    out
}

/// Titles in German or English quotes (`„…“`, `"…"`, `“…”`, `«…»`).
fn quoted_titles(text: &str) -> Vec<String> {
    let pairs = [('„', '“'), ('"', '"'), ('“', '”'), ('«', '»'), ('»', '«')];
    let mut out = Vec::new();
    for (open, close) in pairs {
        let mut rest = text;
        while let Some(start) = rest.find(open) {
            let after = &rest[start + open.len_utf8()..];
            let Some(end) = after.find(close) else { break };
            out.push(after[..end].trim().to_owned());
            rest = &after[end + close.len_utf8()..];
        }
    }
    out
}

/// Every quoted title is the target or in the history.
pub fn is_grounded(reply: &str, target: &Target, history: &[HistoryEntry]) -> bool {
    quoted_titles(reply).iter().all(|q| {
        q.eq_ignore_ascii_case(&target.title)
            || history.iter().any(|h| h.title.eq_ignore_ascii_case(q))
    })
}

/// An explanation and whether the LLM wrote it.
#[derive(Debug, Clone, PartialEq)]
pub struct Explanation {
    pub text: String,
    pub generated: bool,
}

/// LLM-phrased when possible and grounded, otherwise rule-based; `None` without
/// related history.
pub async fn explain(
    client: Option<&LlmClient>,
    target: &Target,
    history: &[HistoryEntry],
) -> Option<Explanation> {
    let fallback = rule_based(target, history)?;
    if let Some(client) = client {
        let mut request = ChatRequest::new(vec![
            Message::system(prompts::EXPLANATION),
            Message::user(describe(target, history)),
        ]);
        request.max_tokens = Some(200);
        match client.chat(&request).await {
            Ok(reply) => {
                let text = strip_thinking(reply.content.as_deref().unwrap_or_default());
                if !text.is_empty() && text.len() < 600 && is_grounded(&text, target, history) {
                    return Some(Explanation {
                        text,
                        generated: true,
                    });
                }
                tracing::debug!(%text, "LLM explanation rejected (not grounded)");
            }
            Err(err) => tracing::warn!(%err, "LLM explanation failed"),
        }
    }
    Some(Explanation {
        text: fallback,
        generated: false,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn target() -> Target {
        Target {
            title: "Zurück in die Zukunft".into(),
            year: Some(1985),
            kind: "movie".into(),
            genres: vec!["Komödie".into(), "Science-Fiction".into()],
            overview: None,
        }
    }

    fn history() -> Vec<HistoryEntry> {
        vec![
            HistoryEntry {
                title: "Zurück in die Zukunft II".into(),
                same_collection: true,
                shared_people: vec!["Michael J. Fox".into()],
                shared_genres: vec!["Komödie".into()],
                ..Default::default()
            },
            HistoryEntry {
                title: "Ghostbusters".into(),
                favorite: true,
                shared_genres: vec!["Komödie".into()],
                ..Default::default()
            },
            HistoryEntry {
                title: "Ohne Bezug".into(),
                ..Default::default()
            },
        ]
    }

    #[test]
    fn the_rule_based_sentence_uses_the_strongest_links() {
        let text = rule_based(&target(), &history()).unwrap();
        assert_eq!(
            text,
            "Weil du „Zurück in die Zukunft II“ und „Ghostbusters“ gesehen hast \
             – ebenfalls mit Michael J. Fox, aus derselben Reihe, auch Komödie."
        );
        assert!(rule_based(&target(), &history()[2..]).is_none());
    }

    #[test]
    fn invented_titles_are_rejected() {
        let h = history();
        assert!(is_grounded(
            "Weil du „Ghostbusters“ mochtest.",
            &target(),
            &h
        ));
        assert!(!is_grounded("Weil du „Matrix“ mochtest.", &target(), &h));
        assert!(!is_grounded(
            "Weil du \"Geheimfilm\" gesehen hast.",
            &target(),
            &h
        ));
        assert!(is_grounded("Ohne Titel.", &target(), &h));
    }
}
