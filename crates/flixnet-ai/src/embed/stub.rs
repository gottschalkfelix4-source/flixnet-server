//! A tiny deterministic embedder for tests (`ai.embedder = "stub"`): feature hashing
//! of word stems plus a small German/English concept lexicon, so "Zeitreise" and
//! "time travel" land close together. No model, no download, microseconds per text.
//! Never meant for real libraries.

use crate::math::normalize;

pub const DIMENSIONS: usize = 256;
pub const MODEL_KEY: &str = "stub-v1";

/// Concepts the stub "understands": any of the fragments in a text adds the concept.
/// Fragments are matched against the folded text (lowercase, umlauts as ae/oe/ue).
const CONCEPTS: &[(&str, &[&str])] = &[
    (
        "time_travel",
        &[
            "zeitreis",
            "zeitmaschin",
            "time travel",
            "time machine",
            "zeitschleife",
            "time loop",
            "zeitsprung",
        ],
    ),
    (
        "dinosaur",
        &[
            "dinosaur",
            "saurier",
            "t-rex",
            "trex",
            "jurassic",
            "kreidezeit",
            "urzeit",
        ],
    ),
    (
        "space",
        &[
            "weltraum",
            "weltall",
            "raumschiff",
            "raumstation",
            "astronaut",
            "spaceship",
            "outer space",
            "galax",
            "planet",
            "mars",
        ],
    ),
    (
        "robot",
        &[
            "roboter",
            "robot",
            "android",
            "kuenstliche intelligenz",
            "artificial intelligence",
            "cyborg",
        ],
    ),
    (
        "war",
        &[
            "krieg", "soldat", "front", "war ", "soldier", "battle", "schlacht",
        ],
    ),
    (
        "love",
        &[
            "liebe", "verlieb", "romanz", "love", "romance", "hochzeit", "wedding",
        ],
    ),
    (
        "crime",
        &[
            "mord",
            "moerder",
            "detektiv",
            "kommissar",
            "ermittl",
            "murder",
            "detective",
            "crime",
            "krimi",
            "verbrech",
        ],
    ),
    (
        "magic",
        &[
            "magie", "zauber", "hexe", "magic", "wizard", "witch", "drache", "dragon",
        ],
    ),
    (
        "sea",
        &[
            "ozean",
            "meer",
            "unterwasser",
            "u-boot",
            "ocean",
            "submarine",
            "hai",
            "shark",
            "schiff",
            "ship",
        ],
    ),
    (
        "horror",
        &[
            "horror", "geist", "spuk", "ghost", "haunted", "daemon", "demon", "zombie", "vampir",
            "vampire",
        ],
    ),
    (
        "comedy",
        &[
            "komoedie",
            "comedy",
            "lustig",
            "funny",
            "chaos",
            "turbulent",
        ],
    ),
    (
        "family",
        &[
            "familie",
            "kinder",
            "family",
            "children",
            "kids",
            "animation",
            "zeichentrick",
        ],
    ),
    (
        "heist",
        &[
            "raub",
            "einbruch",
            "heist",
            "bank robbery",
            "coup",
            "tresor",
        ],
    ),
    (
        "sports",
        &[
            "fussball", "football", "soccer", "boxen", "boxing", "sport", "rennen", "racing",
        ],
    ),
    (
        "music",
        &[
            "musik", "band", "saenger", "singer", "music", "konzert", "concert",
        ],
    ),
];

const STOPWORDS: &[&str] = &[
    "der",
    "die",
    "das",
    "den",
    "dem",
    "des",
    "ein",
    "eine",
    "einer",
    "einen",
    "einem",
    "und",
    "oder",
    "mit",
    "von",
    "fuer",
    "ueber",
    "auf",
    "aus",
    "bei",
    "nach",
    "zum",
    "zur",
    "im",
    "in",
    "ist",
    "sich",
    "wie",
    "wo",
    "film",
    "filme",
    "serie",
    "the",
    "a",
    "an",
    "of",
    "and",
    "or",
    "with",
    "about",
    "for",
    "to",
    "on",
    "in",
    "is",
    "movie",
    "series",
    "show",
    "passage",
    "query",
    "genres",
    "schlagwoerter",
    "mit",
    "regie",
    "jahr",
];

/// Lowercase, umlauts folded, everything but letters/digits/hyphen as space.
fn fold(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for c in text.chars().flat_map(char::to_lowercase) {
        match c {
            'ä' => out.push_str("ae"),
            'ö' => out.push_str("oe"),
            'ü' => out.push_str("ue"),
            'ß' => out.push_str("ss"),
            'é' | 'è' | 'ê' => out.push('e'),
            c if c.is_alphanumeric() || c == '-' => out.push(c),
            _ => out.push(' '),
        }
    }
    out
}

/// Crude stemming so "Zeitreisen"/"Zeitreise" and "robots"/"robot" share a token.
fn stem(word: &str) -> &str {
    for suffix in ["ungen", "ern", "en", "er", "es", "e", "n", "s"] {
        if word.len() > suffix.len() + 3
            && let Some(stem) = word.strip_suffix(suffix)
        {
            return stem;
        }
    }
    word
}

fn fnv(text: &str) -> u64 {
    text.bytes().fold(0xcbf2_9ce4_8422_2325, |h, b| {
        (h ^ b as u64).wrapping_mul(0x0100_0000_01b3)
    })
}

fn add(vector: &mut [f32], feature: &str, weight: f32) {
    let h = fnv(feature);
    let index = (h % DIMENSIONS as u64) as usize;
    let sign = if h >> 63 == 0 { 1.0 } else { -1.0 };
    vector[index] += sign * weight;
}

/// Embeds one text (prefixes such as `passage: ` are just stop words here).
pub fn embed(text: &str) -> Vec<f32> {
    let folded = fold(text);
    let mut vector = vec![0.0_f32; DIMENSIONS];
    for word in folded.split_whitespace() {
        let word = word.trim_matches('-');
        if word.len() < 2 || STOPWORDS.contains(&word) {
            continue;
        }
        add(&mut vector, stem(word), 1.0);
    }
    let spaced = format!(" {folded} ");
    for (concept, fragments) in CONCEPTS {
        if fragments.iter().any(|f| spaced.contains(f)) {
            add(&mut vector, concept, 4.0);
        }
    }
    normalize(&mut vector);
    vector
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::math::dot;

    #[test]
    fn stub_vectors_are_deterministic_unit_vectors() {
        let a = embed("passage: Jurassic Park. Dinosaurier auf einer Insel.");
        assert_eq!(a.len(), DIMENSIONS);
        assert!((crate::math::norm(&a) - 1.0).abs() < 1e-5);
        assert_eq!(
            a,
            embed("passage: Jurassic Park. Dinosaurier auf einer Insel.")
        );
        assert!(embed("").iter().all(|x| *x == 0.0));
    }

    #[test]
    fn concepts_bridge_languages_and_word_forms() {
        let query = embed("query: Film über Zeitreisen mit Dinosauriern");
        let dino = embed(
            "passage: Ein Forscher reist mit einer Zeitmaschine in die Kreidezeit zum T-Rex.",
        );
        let romance = embed("passage: Zwei Fremde verlieben sich in Paris.");
        assert!(dot(&query, &dino) > dot(&query, &romance) + 0.3);
        let english = embed("query: time travel dinosaur movie");
        assert!(dot(&english, &dino) > 0.4, "{}", dot(&english, &dino));
    }
}
