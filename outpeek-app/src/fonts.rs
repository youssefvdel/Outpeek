//! Bundled display fonts, embedded so the app looks identical on every
//! machine.
//!
//! The design tokens name Outfit (display), DM Sans (body) and a mono face.
//! Relying on the system to provide them means Windows silently falls back to
//! Segoe UI and every measurement in the UI is wrong. The files are OFL and
//! ship inside the binary — no installer step, no `<exe>/fonts` directory to
//! lose. Licences live next to the files they cover.

use slint::fontique_011::fontique;

/// Every bundled face. Adding a weight means adding it here AND in
/// `tokens.slint`'s font-family list if the family changes.
const FACES: &[(&str, &[u8])] = &[
    ("Outfit-400", include_bytes!("../assets/fonts/Outfit-400.ttf")),
    ("Outfit-500", include_bytes!("../assets/fonts/Outfit-500.ttf")),
    ("Outfit-600", include_bytes!("../assets/fonts/Outfit-600.ttf")),
    ("Outfit-700", include_bytes!("../assets/fonts/Outfit-700.ttf")),
    ("DMSans-400", include_bytes!("../assets/fonts/DMSans-400.ttf")),
    ("DMSans-500", include_bytes!("../assets/fonts/DMSans-500.ttf")),
    ("DMSans-700", include_bytes!("../assets/fonts/DMSans-700.ttf")),
];

/// Register every embedded face in Slint's shared font collection.
///
/// Returns the number of faces the collection accepted. Must run before the
/// first window is created: Slint resolves family names when it lays text out,
/// and a face registered later would only affect windows created after it.
pub fn register() -> usize {
    let mut registered = 0usize;
    for (name, bytes) in FACES {
        let blob = fontique::Blob::new(std::sync::Arc::new(bytes.to_vec()));
        let mut collection = slint::fontique_011::shared_collection();
        let added = collection.register_fonts(blob, None).len();
        if added == 0 {
            eprintln!("[Outpeek] font {name}: not accepted by the collection");
        }
        registered += added;
    }
    registered
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_bundled_face_is_a_real_ttf() {
        for (name, bytes) in FACES {
            assert!(bytes.len() > 10_000, "{name} looks truncated");
            // TrueType sfnt version: 0x00010000 or 'true'/'ttcf'.
            let head = &bytes[..4];
            assert!(
                head == [0x00, 0x01, 0x00, 0x00] || head == *b"true" || head == *b"ttcf",
                "{name} is not a TrueType file (magic {head:?})"
            );
        }
    }

    #[test]
    fn the_families_the_tokens_ask_for_are_present() {
        let names: Vec<&str> = FACES.iter().map(|(n, _)| *n).collect();
        for want in ["Outfit", "DMSans"] {
            assert!(
                names.iter().any(|n| n.starts_with(want)),
                "no {want} face bundled"
            );
        }
    }
}
