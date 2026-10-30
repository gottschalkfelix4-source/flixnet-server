//! Automatic theme collections ("Zeitreisen", "Heist-Filme"): the LLM proposes
//! themes over the catalogue in batches; proposals are validated (only ids of the
//! batch, at least three members, no titles in names or descriptions) and stored as
//! collections of kind `theme`. Who sees which member is decided when collections
//! are read (library access and parental controls), like for every collection —
//! and since names and descriptions never contain titles, a theme cannot reveal a
//! hidden title either.

use std::collections::{HashMap, HashSet};
use std::time::Duration;

use flixnet_core::jobs::{JobContext, JobHandler, Pool};
use flixnet_core::scheduler::TaskDef;
use flixnet_db::Db;
use flixnet_db::collections::{self, CatalogEntry, ThemeCollection};
use serde_json::{Value, json};

use crate::client::{
    ChatRequest, LlmClient, LlmError, Message, ToolChoice, ToolSpec, extract_json_object,
    parse_arguments,
};
use crate::prompts;
use crate::text::mentions_title;

pub const THEMES_TOOL: &str = "propose_themes";
/// Catalogue entries per request.
pub const BATCH_SIZE: usize = 150;
pub const MIN_MEMBERS: usize = 3;
pub const MAX_THEMES: usize = 20;

pub fn themes_tool() -> ToolSpec {
    ToolSpec {
        name: THEMES_TOOL.to_owned(),
        description: "Schlägt thematische Sammlungen über die gelistete Bibliothek vor.".to_owned(),
        parameters: json!({
            "type": "object",
            "properties": {
                "themes": {
                    "type": "array",
                    "maxItems": 8,
                    "items": {
                        "type": "object",
                        "properties": {
                            "name": { "type": "string", "description": "1–4 Wörter, kein Titel" },
                            "description": { "type": "string", "description": "Ein Satz ohne Titel" },
                            "item_ids": { "type": "array", "items": { "type": "integer" }, "minItems": 3 }
                        },
                        "required": ["name", "item_ids"]
                    }
                }
            },
            "required": ["themes"]
        }),
    }
}

/// A validated proposal of one batch.
#[derive(Debug, Clone, PartialEq)]
pub struct Proposal {
    pub name: String,
    pub description: Option<String>,
    pub item_ids: Vec<i64>,
}

/// Stable key of a theme name: lowercase letters and digits joined by `-`.
pub fn slug(name: &str) -> String {
    crate::text::words(name).join("-")
}

/// Validates the arguments of a `propose_themes` call: unknown ids are dropped,
/// names are 1–60 characters without catalogue titles (else the theme is dropped),
/// descriptions naming a title are removed.
pub fn parse_proposals(args: &Value, batch: &[CatalogEntry], titles: &[&str]) -> Vec<Proposal> {
    let allowed: HashSet<i64> = batch.iter().map(|e| e.id).collect();
    let themes = args
        .get("themes")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    themes
        .iter()
        .filter_map(|theme| {
            let name = theme.get("name")?.as_str()?.trim();
            let name: String = name.chars().take(60).collect();
            if name.is_empty()
                || slug(&name).is_empty()
                || mentions_title(&name, titles.iter().copied())
            {
                return None;
            }
            let mut seen = HashSet::new();
            let item_ids: Vec<i64> = theme
                .get("item_ids")?
                .as_array()?
                .iter()
                .filter_map(|v| v.as_i64().or_else(|| v.as_str()?.trim().parse().ok()))
                .filter(|id| allowed.contains(id) && seen.insert(*id))
                .collect();
            let description = theme
                .get("description")
                .and_then(Value::as_str)
                .map(|d| d.trim().chars().take(300).collect::<String>())
                .filter(|d| !d.is_empty() && !mentions_title(d, titles.iter().copied()));
            Some(Proposal {
                name,
                description,
                item_ids,
            })
        })
        .collect()
}

fn batch_text(batch: &[CatalogEntry], known: &[String]) -> String {
    let mut out = String::new();
    if !known.is_empty() {
        out.push_str("Bereits gefundene Themen (bei Passung wiederverwenden): ");
        out.push_str(&known.join(", "));
        out.push_str("\n\n");
    }
    out.push_str("Bibliothek (id | Titel (Jahr) | Genres | Inhalt):\n");
    for e in batch {
        let overview: String = e
            .overview
            .as_deref()
            .unwrap_or_default()
            .chars()
            .take(140)
            .collect();
        out.push_str(&format!(
            "{} | {}{} | {} | {}\n",
            e.id,
            e.title,
            e.year.map(|y| format!(" ({y})")).unwrap_or_default(),
            e.genres.as_deref().unwrap_or_default(),
            overview.replace('\n', " ")
        ));
    }
    out
}

async fn propose_batch(
    client: &LlmClient,
    batch: &[CatalogEntry],
    titles: &[&str],
    known: &[String],
) -> Result<Vec<Proposal>, LlmError> {
    let mut request = ChatRequest::new(vec![
        Message::system(prompts::THEMES),
        Message::user(batch_text(batch, known)),
    ]);
    request.tools = vec![themes_tool()];
    request.tool_choice = ToolChoice::Function(THEMES_TOOL.to_owned());
    request.temperature = 0.4;
    let reply = client.chat(&request).await?;
    let args = match reply.tool_calls.first() {
        Some(call) => parse_arguments(&call.function.arguments),
        None => reply
            .content
            .as_deref()
            .and_then(extract_json_object)
            .map(|v| v.get("arguments").cloned().unwrap_or(v)),
    };
    let Some(args) = args else {
        tracing::warn!("theme proposal not understood");
        return Ok(Vec::new());
    };
    Ok(parse_proposals(&args, batch, titles))
}

/// Proposes themes over the whole catalogue (batch by batch) and merges them by name.
pub async fn propose(
    client: &LlmClient,
    catalog: &[CatalogEntry],
) -> Result<Vec<ThemeCollection>, LlmError> {
    let titles: Vec<&str> = catalog.iter().map(|e| e.title.as_str()).collect();
    let by_id: HashMap<i64, &CatalogEntry> = catalog.iter().map(|e| (e.id, e)).collect();
    // slug → (name, description, members in order)
    let mut merged: Vec<(String, String, Option<String>, Vec<i64>)> = Vec::new();
    let mut failures = 0;
    for batch in catalog.chunks(BATCH_SIZE) {
        let known: Vec<String> = merged.iter().map(|m| m.1.clone()).collect();
        let proposals = match propose_batch(client, batch, &titles, &known).await {
            Ok(p) => p,
            Err(err) => {
                // One failing batch does not lose the others; all failing is an error.
                tracing::warn!(%err, "theme proposal failed for a batch");
                failures += 1;
                if failures == catalog.len().div_ceil(BATCH_SIZE) {
                    return Err(err);
                }
                continue;
            }
        };
        for p in proposals {
            let key = slug(&p.name);
            match merged.iter_mut().find(|m| m.0 == key) {
                Some(existing) => {
                    for id in p.item_ids {
                        if !existing.3.contains(&id) {
                            existing.3.push(id);
                        }
                    }
                    if existing.2.is_none() {
                        existing.2 = p.description;
                    }
                }
                None => merged.push((key, p.name, p.description, p.item_ids)),
            }
        }
    }
    merged.retain(|m| m.3.len() >= MIN_MEMBERS);
    merged.sort_by_key(|m| std::cmp::Reverse(m.3.len()));
    merged.truncate(MAX_THEMES);
    Ok(merged
        .into_iter()
        .map(|(slug, name, overview, ids)| ThemeCollection {
            sort_name: name.to_lowercase(),
            slug,
            name,
            overview,
            members: ids
                .iter()
                .filter_map(|id| by_id.get(id).map(|e| e.identity_key.clone()))
                .collect(),
        })
        .collect())
}

/// Job: regenerate all theme collections (needs a configured LLM; does nothing
/// without one). The catalogue is read first, the LLM asked without any open
/// transaction, and the result written in one transaction.
pub struct GenerateThemeCollections;

impl JobHandler for GenerateThemeCollections {
    const KIND: &'static str = "ai.theme_collections";
    const POOL: Pool = Pool::Io;
    const MAX_ATTEMPTS: i64 = 1;
    type Payload = ();

    async fn run(&self, ctx: JobContext, _: ()) -> anyhow::Result<()> {
        ctx.progress(0.0).await;
        regenerate(ctx.db()).await?;
        Ok(())
    }
}

/// Regenerates the theme collections; `None` when no LLM is configured (nothing is
/// changed then), else the number of themes stored.
pub async fn regenerate(db: &Db) -> anyhow::Result<Option<usize>> {
    let Some(client) = crate::settings::client(db).await? else {
        tracing::info!("theme collections skipped: no LLM configured");
        return Ok(None);
    };
    let catalog = collections::catalog(db).await?;
    if catalog.len() < MIN_MEMBERS {
        return Ok(Some(0));
    }
    let themes = propose(&client, &catalog).await?;
    collections::sync_themes(db, &themes).await?;
    tracing::info!(themes = themes.len(), "theme collections updated");
    Ok(Some(themes.len()))
}

pub const TASK_NAME: &str = "theme_collections";

pub fn tasks() -> Vec<TaskDef> {
    vec![TaskDef::new::<GenerateThemeCollections>(
        TASK_NAME,
        "Themen-Sammlungen mit dem LLM erzeugen (nur mit eingerichtetem LLM)",
        Duration::from_secs(7 * 24 * 60 * 60),
        &(),
    )]
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(id: i64, title: &str) -> CatalogEntry {
        CatalogEntry {
            id,
            identity_key: format!("local:movie:{id}"),
            kind: "movie".into(),
            title: title.into(),
            year: Some(2000),
            overview: None,
            genres: None,
        }
    }

    #[test]
    fn proposals_are_validated() {
        let batch = vec![
            entry(1, "Zeitmaschine"),
            entry(2, "Geheimfilm"),
            entry(3, "Tick Tack"),
        ];
        let titles = ["Zeitmaschine", "Geheimfilm", "Tick Tack"];
        let proposals = parse_proposals(
            &json!({"themes": [
                {"name": "Zeitreisen", "description": "Reisen durch die Zeit.", "item_ids": [1, "3", 99, 1]},
                {"name": "Geheimfilm und Co", "item_ids": [1, 2, 3]},
                {"name": "Spannung", "description": "Wie im Geheimfilm.", "item_ids": [2]},
                {"name": "  ", "item_ids": [1]},
                {"item_ids": [1]}
            ]}),
            &batch,
            &titles,
        );
        assert_eq!(
            proposals,
            vec![
                Proposal {
                    name: "Zeitreisen".into(),
                    description: Some("Reisen durch die Zeit.".into()),
                    item_ids: vec![1, 3],
                },
                Proposal {
                    name: "Spannung".into(),
                    description: None,
                    item_ids: vec![2],
                },
            ]
        );
        assert!(parse_proposals(&json!({"themes": "kaputt"}), &batch, &titles).is_empty());
        assert_eq!(slug("Heist-Filme!"), "heist-filme");
    }
}
