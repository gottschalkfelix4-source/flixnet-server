//! Jellyfin ids are GUIDs; Flixnet's are integers per table. A GUID here is a fixed
//! marker, the kind of object and the integer id, so it maps back without a lookup
//! table and stays stable across restarts:
//!
//! `666c786e` (`flxn`) · kind (4 hex) · `0000` · id (16 hex), written without dashes
//! like Jellyfin does (`666c786e00010000000000000000002a` = item 42). Parsing accepts dashes,
//! braces and upper case.

const MAGIC: &str = "666c786e";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    /// Movie, series, season or episode.
    Item = 1,
    /// A library, shown as a view (`CollectionFolder`).
    Library = 2,
    User = 3,
    Person = 4,
    Genre = 5,
    /// A media file (`MediaSourceId`).
    File = 6,
    /// A studio (name hash; only for display, not resolvable).
    Studio = 7,
    /// A device (Jellyfin session id).
    Device = 8,
}

impl Kind {
    fn from_code(code: u16) -> Option<Self> {
        Some(match code {
            1 => Self::Item,
            2 => Self::Library,
            3 => Self::User,
            4 => Self::Person,
            5 => Self::Genre,
            6 => Self::File,
            7 => Self::Studio,
            8 => Self::Device,
            _ => return None,
        })
    }
}

pub fn guid(kind: Kind, id: i64) -> String {
    format!("{MAGIC}{:04x}0000{:016x}", kind as u16, id as u64)
}

/// The kind and id behind a GUID this module made.
pub fn parse(value: &str) -> Option<(Kind, i64)> {
    let hex: String = value
        .chars()
        .filter(|c| !matches!(c, '-' | '{' | '}'))
        .collect::<String>()
        .to_ascii_lowercase();
    if hex.len() != 32 || !hex.bytes().all(|b| b.is_ascii_hexdigit()) {
        return None;
    }
    if &hex[..8] != MAGIC || &hex[12..16] != "0000" {
        return None;
    }
    let kind = Kind::from_code(u16::from_str_radix(&hex[8..12], 16).ok()?)?;
    let id = u64::from_str_radix(&hex[16..], 16).ok()? as i64;
    (id > 0).then_some((kind, id))
}

/// The id if the GUID is of the expected kind.
pub fn parse_as(value: &str, kind: Kind) -> Option<i64> {
    parse(value).filter(|(k, _)| *k == kind).map(|(_, id)| id)
}

/// A stable, positive pseudo id for names without a table id (studios).
pub fn name_id(name: &str) -> i64 {
    use sha2::{Digest, Sha256};
    let digest = Sha256::digest(name.to_lowercase().as_bytes());
    let mut bytes = [0u8; 8];
    bytes.copy_from_slice(&digest[..8]);
    (u64::from_be_bytes(bytes) >> 1).max(1) as i64
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn guids_round_trip() {
        let g = guid(Kind::Item, 42);
        assert_eq!(g, "666c786e00010000000000000000002a");
        assert_eq!(g.len(), 32);
        assert_eq!(parse(&g), Some((Kind::Item, 42)));
        let dashed = format!(
            "{}-{}-{}-{}-{}",
            &g[..8],
            &g[8..12],
            &g[12..16],
            &g[16..20],
            &g[20..]
        )
        .to_uppercase();
        assert_eq!(parse(&dashed), Some((Kind::Item, 42)));
        assert_eq!(parse_as(&guid(Kind::Library, 3), Kind::Library), Some(3));
        assert_eq!(parse_as(&guid(Kind::Library, 3), Kind::Item), None);
        assert_eq!(
            parse(&guid(Kind::File, i64::MAX)),
            Some((Kind::File, i64::MAX))
        );
    }

    #[test]
    fn foreign_guids_are_rejected() {
        assert_eq!(parse("f27caa37e5142225cceded48f6553502"), None);
        assert_eq!(
            parse("666c786e00090000000000000000002a"),
            None,
            "unknown kind"
        );
        assert_eq!(parse("666c786e00010001000000000000002a"), None);
        assert_eq!(parse("666c786e000100000000000000000000"), None, "id 0");
        assert_eq!(parse("../../etc/passwd"), None);
        assert_eq!(parse(""), None);
    }

    #[test]
    fn name_ids_are_stable() {
        assert_eq!(name_id("Netflix"), name_id("netflix"));
        assert!(name_id("HBO") > 0);
    }
}
