//! Age ratings (Altersfreigaben): certifications of the common rating systems mapped
//! to the minimum age a profile's limit is compared with.
//!
//! A certification comes from TMDB (the configured country, US as fallback) or from
//! an NFO file (`<mpaa>`), in many spellings: `FSK 12`, `de/16`, `DE:FSK18`,
//! `Germany:12`, `Rated PG-13`, `UK:15`, `TV-MA`, `-12` (France) … The country is
//! taken from a prefix if there is one, else from the hint (the country a TMDB
//! certification was looked up for), else the label is matched against the labels
//! that are unique enough to need no country.
//!
//! Where a system states an age it is used (`PG-13` → 13, `R` → 17, `TV-14` → 14);
//! labels without one are mapped to the nearest common step (`PG` → 6). Unknown
//! labels and "not rated" yield `None`, so restricted profiles do not see them
//! (unless the profile allows unrated content).

/// Minimum age for a certification; `country` (ISO 3166-1, e.g. `DE`) is the system
/// the label belongs to when it carries no country prefix itself.
pub fn age_for(certification: &str, country: Option<&str>) -> Option<i64> {
    let upper = certification.trim().to_uppercase();
    let upper = upper
        .strip_prefix("RATED ")
        .unwrap_or(&upper)
        .trim()
        .to_owned();
    if upper.is_empty() || is_unrated(&upper) {
        return None;
    }
    let (prefix_country, label) = split_country(&upper);
    let label = strip_system(label.trim());
    if label.is_empty() || is_unrated(label) {
        return None;
    }
    let country =
        prefix_country.or_else(|| country.and_then(|c| canonical_country(&c.to_uppercase())));
    if let Some(country) = country
        && let Some(age) = by_country(country, label)
    {
        return Some(age);
    }
    generic(label)
}

/// Labels meaning "no rating".
fn is_unrated(label: &str) -> bool {
    matches!(
        label,
        "NR" | "N/A"
            | "NA"
            | "UR"
            | "UNRATED"
            | "NOT RATED"
            | "NOTRATED"
            | "UNBEKANNT"
            | "OHNE"
            | "-"
            | "E"
            | "EXEMPT"
            | "TBA"
            | "TBC"
            | "?"
    )
}

/// ISO code for the country spellings found in NFO files and TMDB.
fn canonical_country(c: &str) -> Option<&'static str> {
    Some(match c {
        "DE" | "DEU" | "GER" | "GERMANY" | "DEUTSCHLAND" => "DE",
        "AT" | "AUT" | "AUSTRIA" | "ÖSTERREICH" | "OESTERREICH" => "AT",
        "CH" | "CHE" | "SWITZERLAND" | "SCHWEIZ" => "CH",
        "US" | "USA" | "UNITED STATES" => "US",
        "GB" | "UK" | "GBR" | "UNITED KINGDOM" | "GREAT BRITAIN" => "GB",
        "IE" | "IRL" | "IRELAND" => "IE",
        "FR" | "FRA" | "FRANCE" => "FR",
        "NL" | "NLD" | "NETHERLANDS" | "NIEDERLANDE" => "NL",
        "BE" | "BEL" | "BELGIUM" => "BE",
        "ES" | "ESP" | "SPAIN" => "ES",
        "IT" | "ITA" | "ITALY" => "IT",
        "PT" | "PRT" | "PORTUGAL" => "PT",
        "DK" | "DNK" | "DENMARK" => "DK",
        "SE" | "SWE" | "SWEDEN" => "SE",
        "NO" | "NOR" | "NORWAY" => "NO",
        "FI" | "FIN" | "FINLAND" => "FI",
        "PL" | "POL" | "POLAND" => "PL",
        "CA" | "CAN" | "CANADA" => "CA",
        "AU" | "AUS" | "AUSTRALIA" => "AU",
        "NZ" | "NZL" | "NEW ZEALAND" => "NZ",
        "BR" | "BRA" | "BRAZIL" => "BR",
        "JP" | "JPN" | "JAPAN" => "JP",
        "KR" | "KOR" | "SOUTH KOREA" | "KOREA" => "KR",
        _ => return None,
    })
}

/// `DE:16`, `de/16`, `DE-16`, `Germany: 12`, `UK 15` → (country, rest). Only known
/// countries count, so `PG-13` or `TV-MA` are never split.
fn split_country(s: &str) -> (Option<&'static str>, &str) {
    if let Some(pos) = s.find([':', '/'])
        && let Some(c) = canonical_country(s[..pos].trim())
    {
        return (Some(c), &s[pos + 1..]);
    }
    for sep in ['-', ' '] {
        if let Some(pos) = s.find(sep)
            && let Some(c) = canonical_country(&s[..pos])
        {
            return (Some(c), &s[pos + 1..]);
        }
    }
    (None, s)
}

/// System names in front of the label: `FSK 16`, `BBFC 15`, `MPAA R`, `KIJKWIJZER 12`.
fn strip_system(label: &str) -> &str {
    for system in [
        "FSK",
        "FSF",
        "JMK",
        "BBFC",
        "MPAA",
        "KIJKWIJZER",
        "CNC",
        "ICAA",
    ] {
        if let Some(rest) = label.strip_prefix(system) {
            let rest = rest.trim_start_matches([' ', '-', ':']).trim();
            // `FSK` alone (or `FSK ab 12`).
            return rest.strip_prefix("AB ").unwrap_or(rest).trim();
        }
    }
    label.strip_prefix("AB ").unwrap_or(label).trim()
}

/// A plain age: `12`, `16+`, `-12`, `K-16`, `VM14`, `R18+` style numbers. Only for
/// systems whose labels are ages.
fn number(label: &str) -> Option<i64> {
    let digits: String = label
        .trim_start_matches(['-', '+'])
        .trim_end_matches(['+', ' '])
        .trim_start_matches(|c: char| c.is_ascii_alphabetic() || c == '-' || c == ' ')
        .chars()
        .take_while(char::is_ascii_digit)
        .collect();
    let age: i64 = digits.parse().ok()?;
    (0..=21).contains(&age).then_some(age)
}

/// Labels of one country's system.
fn by_country(country: &str, l: &str) -> Option<i64> {
    match country {
        // FSK 0/6/12/16/18; "ohne Altersbeschränkung" = 0.
        "DE" => match l {
            "O.AL." | "OA" | "FREIGEGEBEN OHNE ALTERSBESCHRÄNKUNG" => Some(0),
            "KJ" | "SPIO/JK" | "SPIO" => Some(18),
            _ => number(l),
        },
        // Austria (JMK / FSK): 0, 6, 10, 12, 14, 16, 18.
        "AT" | "CH" | "PL" | "PT" | "NO" | "DK" | "SE" | "FI" | "BE" => match l {
            "A" | "AL" | "U" | "T" | "BTL" | "S" | "KT" | "ALL" | "TOUS" | "G" => Some(0),
            _ => number(l),
        },
        "US" => match l {
            "G" | "TV-Y" | "TV-G" => Some(0),
            "PG" | "TV-PG" | "M" | "GP" => Some(6),
            "TV-Y7" | "TV-Y7-FV" => Some(7),
            "PG-13" => Some(13),
            "TV-14" => Some(14),
            "R" | "TV-MA" => Some(17),
            "NC-17" | "X" => Some(18),
            _ => None,
        },
        // BBFC (UK) and IFCO (Ireland).
        "GB" | "IE" => match l {
            "U" | "UC" | "G" => Some(0),
            "PG" => Some(6),
            "12" | "12A" => Some(12),
            "15" | "15A" => Some(15),
            "16" => Some(16),
            "18" | "R18" => Some(18),
            _ => None,
        },
        // CNC: Tous publics, -10, -12, -16, -18.
        "FR" => match l {
            "U" | "TP" | "TOUS PUBLICS" | "TOUT PUBLIC" => Some(0),
            _ => number(l),
        },
        // Kijkwijzer: AL, 6, 9, 12, 14, 16, 18.
        "NL" => match l {
            "AL" | "ALLE LEEFTIJDEN" => Some(0),
            _ => number(l),
        },
        // ICAA: A/APTA/TP, 7, 12, 16, 18, X.
        "ES" => match l {
            "A" | "APTA" | "TP" | "AI" => Some(0),
            "X" => Some(18),
            _ => number(l),
        },
        // Italy: T, 6+, 14+, 18+ (since 2021); VM14/VM18 before.
        "IT" => match l {
            "T" => Some(0),
            _ => number(l),
        },
        // Canada (home video / provincial): G, PG, 14A, 18A, R, A.
        "CA" => match l {
            "G" | "C" | "C8" => Some(0),
            "PG" => Some(6),
            "14A" | "14+" => Some(14),
            "18A" | "18+" | "R" | "A" => Some(18),
            _ => number(l),
        },
        // Australia: G, PG, M, MA15+, R18+, X18+.
        "AU" => match l {
            "G" => Some(0),
            "PG" => Some(6),
            "M" | "MA15+" | "MA 15+" | "MA" => Some(15),
            "R18+" | "R 18+" | "R" | "X18+" | "X" => Some(18),
            _ => None,
        },
        // New Zealand: G, PG, M, R13, R15, R16, R18.
        "NZ" => match l {
            "G" => Some(0),
            "PG" => Some(6),
            "M" => Some(13),
            "R" => Some(18),
            _ => number(l),
        },
        // Brazil: L, 10, 12, 14, 16, 18.
        "BR" => match l {
            "L" | "ER" => Some(0),
            _ => number(l),
        },
        // Japan (Eirin): G, PG12, R15+, R18+.
        "JP" => match l {
            "G" => Some(0),
            _ => number(l),
        },
        // South Korea: ALL, 12, 15, 18 (Restricted Screening).
        "KR" => match l {
            "ALL" | "G" => Some(0),
            "RESTRICTED SCREENING" => Some(18),
            _ => number(l),
        },
        _ => None,
    }
}

/// Labels without a country: the unambiguous ones of the big systems, else a plain
/// number (in most systems the label *is* the age).
fn generic(label: &str) -> Option<i64> {
    match label {
        "G" | "U" | "TV-Y" | "TV-G" | "AL" | "TP" | "APTA" => Some(0),
        "PG" | "TV-PG" => Some(6),
        "TV-Y7" | "TV-Y7-FV" => Some(7),
        "12A" => Some(12),
        "PG-13" => Some(13),
        "TV-14" | "14A" => Some(14),
        "15A" | "MA15+" => Some(15),
        "R" | "TV-MA" => Some(17),
        "NC-17" | "X" | "R18" | "R18+" | "18A" => Some(18),
        _ => number(label),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn age(cert: &str) -> Option<i64> {
        age_for(cert, None)
    }

    #[test]
    fn german_fsk_in_every_spelling() {
        for (cert, expected) in [
            ("FSK 0", 0),
            ("FSK 6", 6),
            ("FSK 12", 12),
            ("FSK 16", 16),
            ("FSK 18", 18),
            ("FSK18", 18),
            ("FSK-16", 16),
            ("FSK ab 12", 12),
            ("de/16", 16),
            ("DE:FSK18", 18),
            ("DE-12", 12),
            ("Germany:12", 12),
            ("Deutschland: 6", 6),
            ("o.Al.", 0),
        ] {
            let got = if cert == "o.Al." {
                age_for(cert, Some("DE"))
            } else {
                age(cert)
            };
            assert_eq!(got, Some(expected), "{cert}");
        }
        assert_eq!(age_for("16", Some("DE")), Some(16));
    }

    #[test]
    fn austria_and_switzerland() {
        for (cert, expected) in [
            ("0", 0),
            ("6", 6),
            ("10", 10),
            ("12", 12),
            ("14", 14),
            ("16", 16),
            ("18", 18),
        ] {
            assert_eq!(age_for(cert, Some("AT")), Some(expected), "AT {cert}");
        }
        assert_eq!(age("AT:14"), Some(14));
        assert_eq!(age_for("7", Some("CH")), Some(7));
        assert_eq!(age("CH:16"), Some(16));
    }

    #[test]
    fn us_mpaa_and_tv_parental_guidelines() {
        for (cert, expected) in [
            ("G", 0),
            ("PG", 6),
            ("PG-13", 13),
            ("R", 17),
            ("NC-17", 18),
            ("Rated R", 17),
            ("Rated PG-13", 13),
            ("USA:PG-13", 13),
            ("US:R", 17),
            ("TV-Y", 0),
            ("TV-Y7", 7),
            ("TV-G", 0),
            ("TV-PG", 6),
            ("TV-14", 14),
            ("TV-MA", 17),
            ("MPAA R", 17),
        ] {
            assert_eq!(age(cert), Some(expected), "{cert}");
            assert_eq!(age_for(cert, Some("US")), Some(expected), "US {cert}");
        }
    }

    #[test]
    fn uk_bbfc_and_ireland() {
        for (cert, expected) in [
            ("U", 0),
            ("PG", 6),
            ("12A", 12),
            ("12", 12),
            ("15", 15),
            ("18", 18),
            ("R18", 18),
        ] {
            assert_eq!(age_for(cert, Some("GB")), Some(expected), "GB {cert}");
        }
        assert_eq!(age("UK:15"), Some(15));
        assert_eq!(age("GB-15"), Some(15));
        assert_eq!(age("BBFC 12A"), Some(12));
        assert_eq!(age_for("15A", Some("IE")), Some(15));
        assert_eq!(age_for("16", Some("IE")), Some(16));
    }

    #[test]
    fn france_netherlands_spain_italy() {
        for (cert, expected) in [
            ("U", 0),
            ("TP", 0),
            ("10", 10),
            ("-12", 12),
            ("12", 12),
            ("-16", 16),
            ("18", 18),
        ] {
            assert_eq!(age_for(cert, Some("FR")), Some(expected), "FR {cert}");
        }
        assert_eq!(age("FR:-16"), Some(16));
        for (cert, expected) in [
            ("AL", 0),
            ("6", 6),
            ("9", 9),
            ("12", 12),
            ("14", 14),
            ("16", 16),
            ("18", 18),
        ] {
            assert_eq!(age_for(cert, Some("NL")), Some(expected), "NL {cert}");
        }
        assert_eq!(age("Kijkwijzer 9"), Some(9));
        for (cert, expected) in [
            ("A", 0),
            ("APTA", 0),
            ("7", 7),
            ("12", 12),
            ("16", 16),
            ("18", 18),
            ("X", 18),
        ] {
            assert_eq!(age_for(cert, Some("ES")), Some(expected), "ES {cert}");
        }
        for (cert, expected) in [
            ("T", 0),
            ("6+", 6),
            ("14+", 14),
            ("18+", 18),
            ("VM14", 14),
            ("VM18", 18),
        ] {
            assert_eq!(age_for(cert, Some("IT")), Some(expected), "IT {cert}");
        }
    }

    #[test]
    fn other_systems() {
        assert_eq!(age_for("MA15+", Some("AU")), Some(15));
        assert_eq!(age_for("M", Some("AU")), Some(15));
        assert_eq!(age_for("R18+", Some("AU")), Some(18));
        assert_eq!(age_for("14A", Some("CA")), Some(14));
        assert_eq!(age_for("18A", Some("CA")), Some(18));
        assert_eq!(age_for("R16", Some("NZ")), Some(16));
        assert_eq!(age_for("R13", Some("NZ")), Some(13));
        assert_eq!(age_for("L", Some("BR")), Some(0));
        assert_eq!(age_for("14", Some("BR")), Some(14));
        assert_eq!(age_for("PG12", Some("JP")), Some(12));
        assert_eq!(age_for("R15+", Some("JP")), Some(15));
        assert_eq!(age_for("15", Some("KR")), Some(15));
        assert_eq!(age_for("K-16", Some("FI")), Some(16));
        assert_eq!(age_for("11", Some("SE")), Some(11));
        assert_eq!(age_for("Btl", Some("SE")), Some(0));
        assert_eq!(age_for("A", Some("DK")), Some(0));
        assert_eq!(age_for("15", Some("NO")), Some(15));
    }

    #[test]
    fn the_prefix_beats_the_hint() {
        // A US label in a German setup, and a British "15" where FSK has no 15.
        assert_eq!(age_for("US:PG-13", Some("DE")), Some(13));
        assert_eq!(age_for("UK:15", Some("DE")), Some(15));
        // Labels of the hint's system first: Australia's M is 15, the US' M is 6.
        assert_eq!(age_for("M", Some("AU")), Some(15));
        assert_eq!(age_for("M", Some("US")), Some(6));
    }

    #[test]
    fn unrated_and_unknown_labels_have_no_age() {
        for cert in [
            "",
            "  ",
            "NR",
            "Not Rated",
            "Unrated",
            "Unbekannt",
            "N/A",
            "FSK",
            "DE:",
            "Banned",
            "99",
        ] {
            assert_eq!(age(cert), None, "{cert:?}");
        }
        assert_eq!(age_for("Q", Some("US")), None);
    }

    #[test]
    fn labels_that_look_like_countries_are_not_split() {
        // PG = Papua New Guinea, MA = Morocco, TV = Tuvalu: not supported countries.
        assert_eq!(age("PG-13"), Some(13));
        assert_eq!(age("TV-MA"), Some(17));
        assert_eq!(age("MA15+"), Some(15));
    }
}
