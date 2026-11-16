//! Badge choice for the artwork ported from the React app.
//!
//! The files live in `ui/assets/badges/` and Slint embeds them at compile time
//! (`@image-url`); this module owns only *which* one is shown. The `.slint` side
//! branches on the key returned here and never decides a tier itself, so the
//! grade rule is testable on a host with no UI.
//!
//! Combat Highlights has no rule: the three round badges are fixed art per row
//! (`cards.slint`).

/// One Tracker Score medallion grade, in the React app's `SCORE_TIERS` order.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ScoreBadge {
    OneK,
    S,
    A,
    B,
    C,
    D,
}

impl ScoreBadge {
    /// Asset filename under `ui/assets/badges/`.
    pub fn asset(self) -> &'static str {
        match self {
            ScoreBadge::OneK => "score-1k.svg",
            ScoreBadge::S => "score-s.svg",
            ScoreBadge::A => "score-a.svg",
            ScoreBadge::B => "score-b.svg",
            ScoreBadge::C => "score-c.svg",
            ScoreBadge::D => "score-d.svg",
        }
    }

    /// The string `Tracker.score-badge` carries, and the guard `cards.slint`
    /// branches on. Also the tier letter the React app shows in a chip/tooltip.
    pub fn key(self) -> &'static str {
        match self {
            ScoreBadge::OneK => "1K",
            ScoreBadge::S => "S",
            ScoreBadge::A => "A",
            ScoreBadge::B => "B",
            ScoreBadge::C => "C",
            ScoreBadge::D => "D",
        }
    }

    /// Every grade, for the asset-presence test.
    pub const ALL: [ScoreBadge; 6] = [
        ScoreBadge::OneK,
        ScoreBadge::S,
        ScoreBadge::A,
        ScoreBadge::B,
        ScoreBadge::C,
        ScoreBadge::D,
    ];
}

/// Grade → medallion, mirroring `src/components/ScoreBadge.tsx` exactly:
///
/// * `SCORE_TIERS` puts `1K` first with `min: 1000`, and `scoreTier()` picks
///   the first band the numeric score clears — so a score of 1000 earns the 1K
///   emblem even if the caller passes a letter grade. 999 is still S.
/// * `gradeFor()` already returns `1K` for the top percentile band (`top <= 1`),
///   which is the same emblem, so a `1K` letter is honoured too.
/// * Letters are compared case-insensitively: a scraped `"s"` is not a reason
///   to fall back to the shield.
/// * A numeric score with no letter (Tracker Network's performance score has
///   none) is banded through the rest of React's `SCORE_TIERS`, so 148 is a D
///   medal rather than an UNRATED shield beside a real number.
///
/// Unknown or empty grade with no score → `None`, the honest UNRATED state.
/// React's `?? scoreS` fallback would paint an S medal over an unknown tier;
/// this build keeps the drawn shield instead of guessing.
pub fn score_badge(grade: &str, score: Option<i32>) -> Option<ScoreBadge> {
    // React: `SCORE_TIERS.find((t) => score >= t.min)`, 1K band first.
    if score.is_some_and(|s| s >= 1000) {
        return Some(ScoreBadge::OneK);
    }
    let letter = match grade.trim().to_ascii_uppercase().as_str() {
        "1K" => Some(ScoreBadge::OneK),
        "S" => Some(ScoreBadge::S),
        "A" => Some(ScoreBadge::A),
        "B" => Some(ScoreBadge::B),
        "C" => Some(ScoreBadge::C),
        "D" => Some(ScoreBadge::D),
        _ => None,
    };
    if letter.is_some() {
        return letter;
    }
    // No letter, but a number: Tracker Network's performance score carries no
    // letter grade, and React's `scoreTier()` maps the number through the rest
    // of `SCORE_TIERS`. A letter still wins below 1K, so a scraped grade is
    // never overridden by a rounded score.
    score.and_then(score_band)
}

/// React `SCORE_TIERS` below the 1K band, as `Option` so an out-of-range or
/// negative score declares no tier instead of the D fallback.
fn score_band(score: i32) -> Option<ScoreBadge> {
    if score < 0 {
        return None;
    }
    Some(match score {
        1000.. => ScoreBadge::OneK,
        825.. => ScoreBadge::S,
        650.. => ScoreBadge::A,
        475.. => ScoreBadge::B,
        300.. => ScoreBadge::C,
        _ => ScoreBadge::D,
    })
}

/// One-liner for `Tracker.set_score_badge`: the key, or `""` for unrated.
pub fn badge_key(grade: &str, score: Option<i32>) -> &'static str {
    score_badge(grade, score).map_or("", ScoreBadge::key)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    /// The UI source, compiled in as text: the asset test proves the files the
    /// `.slint` side names exist, not just that some file exists somewhere.
    const CARDS: &str = include_str!("../ui/components/cards.slint");
    const ASSET_DIR: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/ui/assets/badges");

    fn asset_bytes(name: &str) -> Vec<u8> {
        let path = PathBuf::from(ASSET_DIR).join(name);
        std::fs::read(&path).unwrap_or_else(|e| panic!("{}: {e}", path.display()))
    }

    fn slint_reference(name: &str) -> String {
        format!("@image-url(\"../assets/badges/{name}\")")
    }

    #[test]
    fn every_letter_grade_gets_its_own_medallion() {
        for (grade, badge) in [
            ("S", ScoreBadge::S),
            ("A", ScoreBadge::A),
            ("B", ScoreBadge::B),
            ("C", ScoreBadge::C),
            ("D", ScoreBadge::D),
        ] {
            assert_eq!(score_badge(grade, None), Some(badge), "grade {grade}");
        }
    }

    #[test]
    fn lowercase_grades_are_the_same_grade() {
        for badge in ScoreBadge::ALL {
            let lower = badge.key().to_lowercase();
            assert_eq!(score_badge(&lower, None), Some(badge), "grade {lower}");
            assert_eq!(badge_key(&lower, None), badge.key());
        }
    }

    #[test]
    fn unknown_or_empty_grade_stays_unrated() {
        for grade in ["", " ", "X", "SS", "1", "gold"] {
            assert_eq!(score_badge(grade, None), None, "grade {grade:?}");
            assert_eq!(badge_key(grade, None), "");
        }
    }

    #[test]
    fn a_score_of_1000_earns_the_1k_emblem() {
        // React `SCORE_TIERS`: 1K min 1000, S min 825. The numeric band wins
        // over the letter, and 999 is still S.
        assert_eq!(score_badge("S", Some(999)), Some(ScoreBadge::S));
        assert_eq!(score_badge("S", Some(1000)), Some(ScoreBadge::OneK));
        assert_eq!(score_badge("D", Some(1500)), Some(ScoreBadge::OneK));
        // A 1K letter (gradeFor's top percentile band) needs no number.
        assert_eq!(score_badge("1K", None), Some(ScoreBadge::OneK));
        assert_eq!(score_badge("1k", Some(0)), Some(ScoreBadge::OneK));
    }

    #[test]
    fn every_medallion_asset_exists_and_cards_slint_embeds_it() {
        for badge in ScoreBadge::ALL {
            let name = badge.asset();
            let bytes = asset_bytes(name);
            assert!(bytes.len() > 500, "{name} looks truncated");
            assert!(
                bytes.starts_with(b"<?xml") && bytes.windows(4).any(|w| w == b"<svg"),
                "{name} is not an SVG"
            );
            assert!(
                CARDS.contains(&slint_reference(name)),
                "cards.slint no longer embeds {name}"
            );
        }
    }

    #[test]
    fn a_score_without_a_letter_is_banded_like_the_react_tiers() {
        // Tracker Network's score has no letter. React's SCORE_TIERS decide.
        for (score, badge) in [
            (0, ScoreBadge::D),
            (148, ScoreBadge::D),
            (299, ScoreBadge::D),
            (300, ScoreBadge::C),
            (475, ScoreBadge::B),
            (650, ScoreBadge::A),
            (825, ScoreBadge::S),
            (999, ScoreBadge::S),
        ] {
            assert_eq!(score_badge("", Some(score)), Some(badge), "score {score}");
        }
        // A letter still wins below 1K; only 1K outranks it.
        assert_eq!(score_badge("A", Some(148)), Some(ScoreBadge::A));
        assert_eq!(score_badge("A", Some(1000)), Some(ScoreBadge::OneK));
        // A negative score is not a band.
        assert_eq!(score_badge("", Some(-1)), None);
        assert_eq!(score_badge("", None), None);
    }

    #[test]
    fn combat_badges_are_the_png_artwork_not_glyphs() {
        for name in ["kills.png", "firstbloods.png", "aces.png"] {
            let bytes = asset_bytes(name);
            assert!(bytes.len() > 500, "{name} looks truncated");
            assert!(
                bytes.starts_with(b"\x89PNG\r\n\x1a\n"),
                "{name} is not a PNG"
            );
            assert!(
                CARDS.contains(&slint_reference(name)),
                "cards.slint no longer embeds {name}"
            );
        }
        // The rows that used to draw a LegacyGlyph now pass the art; the glyph
        // kinds are gone, so they cannot creep back as a drawn stand-in.
        let row = block(CARDS, "component HighlightRow", "component TopAgent");
        assert!(!row.contains("LegacyGlyph"));
        assert!(!row.contains("Glyph {"));
    }

    #[test]
    fn combat_rows_bind_real_values_and_default_to_an_em_dash() {
        let row = block(CARDS, "component HighlightRow", "component TopAgent");
        // The default is empty, and the row draws the dash for it: absence is
        // a dash, never a zero.
        assert!(row.contains(r#"in property <string> value: "";"#));
        assert!(row.contains(r#"text: root.value == "" ? "—" : root.value;"#));

        let card = block(CARDS, "component CombatHighlights", "component TopAgent");
        let calls: Vec<&str> = card.matches("HighlightRow {").collect();
        assert_eq!(calls.len(), 3, "three combat rows");
        // Every row binds a Tracker Network value — the model's, not a literal
        // written here, so the card cannot show a number the read did not make.
        let bindings: Vec<&str> = card
            .split("HighlightRow {")
            .skip(1)
            .map(|call| call.split('}').next().unwrap_or_default())
            .collect();
        for (body, key) in bindings.iter().zip([
            "value: Tracker.best-kills;",
            "value: Tracker.first-blood-deaths;",
            "value: Tracker.aces;",
        ]) {
            assert!(body.contains(key), "row must bind {key}: {body}");
        }
        // The empty state is one quiet word, not a badge on every card: a pill
        // per card was decoration. Nothing is drawn once the read succeeded.
        assert!(
            card.contains(r#"text: Tracker.profile-state == "loading" ? "reading" : "not read""#)
        );
        assert!(!card.contains("Chip {"), "no badge in the combat card");
    }

    /// Text from `start` up to the next `end` marker; panics if either is gone.
    fn block<'a>(source: &'a str, start: &str, end: &str) -> &'a str {
        let from = source
            .find(start)
            .unwrap_or_else(|| panic!("{start} is gone from cards.slint"));
        let rest = &source[from..];
        let to = rest
            .find(end)
            .unwrap_or_else(|| panic!("{end} is gone from cards.slint"));
        &rest[..to]
    }
}
