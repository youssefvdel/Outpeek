//! Equipped weapon-skins viewer: parse what a player is holding.
//!
//! Port of `legacy/src/utils/loadout.ts`. The only endpoints that expose
//! equipped skins are the in-progress loadout routes
//!
//! ```text
//! GET glz-{region}-1.{shard}.a.pvp.net/core-game/v1/matches/{id}/loadouts
//! GET glz-{region}-1.{shard}.a.pvp.net/pregame/v1/matches/{id}/loadouts
//! ```
//!
//! Riot retired the per-player personalization routes (verified 404), so there
//! is no out-of-match equivalent — the viewer is gated on a live match.
//!
//! Payload shape (verified against a live match):
//!
//! ```text
//! Loadouts[]  { CharacterID, Subject, Loadout }
//! Loadout     { Subject, Items, Expressions, DynamicOptions }
//! Items       { [WEAPON_UUID]: { ID, TypeID, Sockets } }
//! Sockets     { ["3ad1b2b2-…"]: { ID, Item: { ID, TypeID } } }
//! ```
//!
//! Three traps, each load-bearing:
//!
//! 1. `Items` is keyed by WEAPON uuid (Classic `29a0cfab-…`, Vandal
//!    `9c82e19d-…`), not by an opaque socket id.
//! 2. The equipped value is NOT `Items[key].ID` (that is the weapon's default
//!    entry). It lives in the fixed [`SKIN_SOCKET`], and that value may be a
//!    CHROMA uuid or a SKIN uuid depending on the weapon — both resolve.
//! 3. `Sprays` is ABSENT from the payload even though older docs list it.
//!    (Spray/flex parsing is out of scope for this port: the viewer shows
//!    weapon rows with per-weapon buddies, not the expressions wheel.)
//!
//! ART SOURCE: `/v1/weapons/skinchromas` is the authoritative per-chroma art
//! table. `/v1/weapons` alone is not enough — the DEFAULT ("Standard …") skin
//! entries carry a GREY X PLACEHOLDER as their `displayIcon`, so a default
//! loadout resolved through skins alone paints an X on every default weapon.
//! When the equipped skin is a default, the parse falls back to the
//! weapon-level `displayIcon` (the canonical gun render), never the placeholder.

use std::collections::HashMap;

use serde_json::Value;

/// The socket holding the equipped weapon skin variant (chroma uuid).
/// Fixed across patches so far.
pub const SKIN_SOCKET: &str = "3ad1b2b2-acdb-4524-8f52-954a76ddae0a";
/// The socket holding the parent skin identity (skin uuid).
pub const SKIN_ID_SOCKET: &str = "bcef87d6-209b-46c6-8b19-fbe40bd95abc";
/// The socket holding the equipped VFX/finisher stage (level uuid).
pub const SKIN_LEVEL_SOCKET: &str = "e7c63390-eda7-46e0-bb7a-a6abdacd2433";
/// The socket holding the equipped gun buddy (absent when unequipped).
pub const BUDDY_SOCKET: &str = "77258665-71d1-4623-bc72-44db9bd5b3b3";

/// Canonical top-to-bottom weapon order, matching the collection screen.
/// Title-case names, as the parse stores them.
pub const WEAPON_ORDER: &[&str] = &[
    "Classic", "Shorty", "Frenzy", "Ghost", "Bandit", "Sheriff", "Stinger", "Spectre", "Bucky",
    "Judge", "Bulldog", "Guardian", "Warden", "Phantom", "Vandal", "Melee", "Marshal", "Outlaw",
    "Operator", "Ares", "Odin",
];

/// Position of a weapon in the collection order; unknown names sort last.
pub fn order_of(weapon_name: &str) -> usize {
    WEAPON_ORDER
        .iter()
        .position(|n| n.eq_ignore_ascii_case(weapon_name))
        .unwrap_or(WEAPON_ORDER.len())
}

/// One weapon: uuid, display name, category and the canonical art.
///
/// `icon` is the default variant's full render when the content API names one,
/// else the weapon-level `displayIcon` — the render default skins draw with
/// instead of the grey-X placeholder.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct WeaponInfo {
    pub uuid: String,
    pub name: String,
    pub category: String,
    pub icon: String,
}

/// One resolvable skin/chroma/level uuid: the parent identity and art.
///
/// `icon` is the chroma/skin `displayIcon`; `full_render` is the full variant
/// gun render and ALWAYS wins for equipped chromas — `displayIcon` can be a
/// swatch, a base-variant render, or missing.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SkinEntry {
    pub weapon_uuid: String,
    pub name: String,
    pub icon: String,
    pub full_render: String,
    pub is_default: bool,
}

/// The content-API catalogue the parse resolves against.
#[derive(Debug, Clone, Default)]
pub struct Catalog {
    pub weapons: HashMap<String, WeaponInfo>,
    pub skin_index: HashMap<String, SkinEntry>,
    /// Chroma uuid -> Riot's raw chroma label, e.g.
    /// "Neptune Odin Level 3 / (Variant 1 Black)". Never shown verbatim: the
    /// viewer shows the parent skin name plus the extracted `(Variant …)` tag.
    pub chroma_names: HashMap<String, String>,
    /// Skin-level uuid -> 1-based level number ("Lv 4"), from `levels[]` order.
    pub level_index: HashMap<String, u32>,
    /// Buddy uuid -> (name, art). The buddy socket is absent when unequipped.
    pub buddies: HashMap<String, (String, String)>,
}

/// One equipped weapon, pre-formatted for the view. Empty strings mean "not
/// reported" and the view draws an em dash — never a zero or invented art.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct EquippedWeapon {
    pub weapon_uuid: String,
    pub weapon_name: String,
    pub category: String,
    pub skin_id: String,
    pub skin_name: String,
    /// Resolved art: variant full render, then skin render, then the canonical
    /// weapon render. Never the grey-X placeholder.
    pub icon: String,
    /// True when the player runs the default (unskinned) weapon entry.
    pub is_default_skin: bool,
    /// Riot's variant tag, e.g. "Variant 1 Black". Empty for base skins.
    pub variant_label: String,
    /// Equipped VFX stage, 0 when unknown.
    pub level: u32,
    pub buddy_name: String,
    pub buddy_icon: String,
}

/// One player's parsed loadout.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct PlayerLoadout {
    /// PUUID — present on the payload and the most reliable join key.
    pub subject: String,
    pub character_id: String,
    pub weapons: Vec<EquippedWeapon>,
}

/// Riot's "(Variant …)" tag out of a raw chroma label like
/// "Neptune Odin Level 3 / (Variant 1 Black)". Empty when the chroma carries
/// no variant marking (base colors).
pub fn variant_label_of(chroma_name: &str) -> String {
    let bytes = chroma_name.as_bytes();
    // Find the last '(' that closes with ')', case-insensitively containing
    // "variant".
    let mut open: Option<usize> = None;
    for (i, b) in bytes.iter().enumerate() {
        if *b == b'(' {
            open = Some(i);
        }
    }
    let Some(start) = open else {
        return String::new();
    };
    let rest = &chroma_name[start + 1..];
    let Some(end) = rest.find(')') else {
        return String::new();
    };
    let inner = rest[..end].trim();
    if inner.to_lowercase().contains("variant") {
        inner.split_whitespace().collect::<Vec<_>>().join(" ")
    } else {
        String::new()
    }
}

/// Riot's "Standard …" entries are default skins. Their `displayIcon` is a grey
/// X placeholder rather than a weapon render, so they must be drawn with the
/// weapon-level art instead.
pub fn is_default_skin_name(skin_name: &str, weapon_name: &str) -> bool {
    let s = skin_name.trim().to_lowercase();
    let w = weapon_name.trim().to_lowercase();
    s.starts_with("standard") || (!w.is_empty() && s == w) || s == "melee"
}

/// Chroma labels are sometimes just the gun ("Ghost") — never a skin name.
fn is_bare_weapon_name(label: &str, weapon_name: &str) -> bool {
    let s = label.trim().to_lowercase();
    let w = weapon_name.trim().to_lowercase();
    !s.is_empty() && (s == w || s == format!("standard {w}"))
}

fn text(value: Option<&Value>) -> String {
    match value {
        Some(Value::String(s)) => s.trim().to_string(),
        Some(v) if v.is_string() => v.as_str().unwrap_or("").trim().to_string(),
        _ => String::new(),
    }
}

/// Case-insensitive field read that tolerates Riot's PascalCase drift.
fn pick(entry: &Value, names: &[&str]) -> String {
    for name in names {
        // Direct hit first, then a case-insensitive scan of the object.
        if let Some(v) = entry.get(*name) {
            let s = text(Some(v));
            if !s.is_empty() {
                return s;
            }
        }
        if let Some(obj) = entry.as_object() {
            for (k, v) in obj {
                if k.eq_ignore_ascii_case(name) {
                    let s = text(Some(v));
                    if !s.is_empty() {
                        return s;
                    }
                }
            }
        }
    }
    String::new()
}

/// Build the catalogue from the content API.
///
/// `weapons` is the `/v1/weapons` array (weapon names, categories, canonical
/// art, skin lists); `chromas` is the `/v1/weapons/skinchromas` array
/// (authoritative per-chroma art — art only, never names: chroma displayNames
/// are raw internal labels and must not replace the parent skin name).
pub fn build_catalog(weapons: &Value, chromas: &Value) -> Catalog {
    let mut catalog = Catalog::default();
    let list = weapons.as_array().cloned().unwrap_or_default();
    for w in &list {
        let Some(uuid) = w.get("uuid").and_then(Value::as_str) else {
            continue;
        };
        let uuid = uuid.to_lowercase();
        let weapon_name = w
            .get("displayName")
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_string();
        let def_uuid = w
            .get("defaultSkinUuid")
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_lowercase();
        let skins = w
            .get("skins")
            .and_then(|v| v.as_array())
            .cloned()
            .unwrap_or_default();
        let def_skin = skins
            .iter()
            .find(|s| {
                s.get("uuid")
                    .and_then(Value::as_str)
                    .map(|u| u.to_lowercase() == def_uuid)
                    .unwrap_or(false)
            })
            .or_else(|| {
                skins.iter().find(|s| {
                    is_default_skin_name(
                        s.get("displayName").and_then(Value::as_str).unwrap_or(""),
                        &weapon_name,
                    )
                })
            })
            .or_else(|| skins.first());
        let def_chroma = def_skin
            .and_then(|s| s.get("chromas"))
            .and_then(Value::as_array)
            .and_then(|c| c.first());
        // Canonical 3D render for default skins (never the X placeholder).
        // The weapon-level `displayIcon` outranks every skin/chroma
        // `displayIcon`: default entries carry the grey-X placeholder there,
        // while the weapon render is the canonical gun.
        let canonical = text(def_chroma.and_then(|c| c.get("fullRender")))
            .move_or(text(w.get("displayIcon")))
            .move_or(text(def_chroma.and_then(|c| c.get("displayIcon"))))
            .move_or(text(def_skin.and_then(|s| s.get("displayIcon"))));
        catalog.weapons.insert(
            uuid.clone(),
            WeaponInfo {
                uuid: uuid.clone(),
                name: weapon_name.clone(),
                category: w
                    .get("category")
                    .and_then(Value::as_str)
                    .unwrap_or("")
                    .to_string(),
                icon: canonical.clone(),
            },
        );
        for s in &skins {
            let Some(skin_uuid) = s.get("uuid").and_then(Value::as_str) else {
                continue;
            };
            let skin_name = s
                .get("displayName")
                .and_then(Value::as_str)
                .unwrap_or(&weapon_name)
                .to_string();
            let def = is_default_skin_name(&skin_name, &weapon_name);
            // Default skins ALWAYS use the canonical render (never the X!).
            let skin_icon = if def {
                canonical.clone()
            } else {
                let own = text(s.get("displayIcon"));
                if own.is_empty() {
                    let first_chroma = s
                        .get("chromas")
                        .and_then(Value::as_array)
                        .and_then(|c| c.first())
                        .map(|c| text(c.get("displayIcon")))
                        .unwrap_or_default();
                    if first_chroma.is_empty() {
                        canonical.clone()
                    } else {
                        first_chroma
                    }
                } else {
                    own
                }
            };
            let entry = SkinEntry {
                weapon_uuid: uuid.clone(),
                name: skin_name,
                icon: skin_icon,
                full_render: String::new(),
                is_default: def,
            };
            catalog
                .skin_index
                .insert(skin_uuid.to_lowercase(), entry.clone());
            for c in s
                .get("chromas")
                .and_then(Value::as_array)
                .cloned()
                .unwrap_or_default()
            {
                let Some(cu) = c.get("uuid").and_then(Value::as_str) else {
                    continue;
                };
                let cu = cu.to_lowercase();
                catalog
                    .skin_index
                    .entry(cu.clone())
                    .or_insert(entry.clone());
                if let Some(cname) = c.get("displayName").and_then(Value::as_str) {
                    if !cname.is_empty() {
                        catalog.chroma_names.entry(cu).or_insert(cname.to_string());
                    }
                }
            }
            if let Some(levels) = s.get("levels").and_then(Value::as_array) {
                for (li, lv) in levels.iter().enumerate() {
                    let Some(lu) = lv.get("uuid").and_then(Value::as_str) else {
                        continue;
                    };
                    let lu = lu.to_lowercase();
                    catalog
                        .skin_index
                        .entry(lu.clone())
                        .or_insert(entry.clone());
                    catalog.level_index.insert(lu, (li + 1) as u32);
                }
            }
        }
    }
    // `/weapons/skinchromas` is authoritative for chroma art and covers chromas
    // `/weapons` omits. Art only — never names.
    for c in chromas.as_array().cloned().unwrap_or_default() {
        let Some(cu) = c.get("uuid").and_then(Value::as_str) else {
            continue;
        };
        let cu = cu.to_lowercase();
        let name = c
            .get("displayName")
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_string();
        let icon = text(c.get("displayIcon"));
        let full = text(c.get("fullRender"));
        if !name.is_empty() {
            catalog
                .chroma_names
                .entry(cu.clone())
                .or_insert(name.clone());
        }
        match catalog.skin_index.get(&cu) {
            Some(existing) if !existing.is_default => {
                let mut merged = existing.clone();
                if !icon.is_empty() {
                    merged.icon = icon;
                }
                if !full.is_empty() {
                    merged.full_render = full;
                }
                catalog.skin_index.insert(cu, merged);
            }
            Some(_) => {
                // A default skin entry: never overwrite with a placeholder icon.
            }
            None => {
                if icon.is_empty() && full.is_empty() {
                    continue;
                }
                let def = is_default_skin_name(&name, "");
                catalog.skin_index.insert(
                    cu,
                    SkinEntry {
                        weapon_uuid: String::new(),
                        name,
                        icon: if def { String::new() } else { icon },
                        full_render: if def { String::new() } else { full },
                        is_default: def,
                    },
                );
            }
        }
    }
    catalog
}

trait MoveOr {
    fn move_or(self, fallback: String) -> String;
}

impl MoveOr for String {
    fn move_or(self, fallback: String) -> String {
        if self.is_empty() {
            fallback
        } else {
            self
        }
    }
}

/// Parse one loadout into the shape the viewer renders.
///
/// `entry` may be the core-game shape (`{ CharacterID, Loadout }`) or the
/// pregame shape (the loadout object itself) — pregame entries are not nested
/// under a `Loadout` key, and mixing them up yields an empty viewer.
pub fn parse_loadout_entry(entry: &Value, catalog: &Catalog) -> PlayerLoadout {
    let character_id = pick(entry, &["CharacterID", "CharacterId", "characterId"]);
    let data = entry
        .get("Loadout")
        .or_else(|| entry.get("loadout"))
        .unwrap_or(entry);
    let subject = pick(data, &["Subject", "subject"]);
    let subject = if subject.is_empty() {
        pick(entry, &["Subject", "subject"])
    } else {
        subject
    };
    let items = data
        .get("Items")
        .or_else(|| data.get("items"))
        .or_else(|| entry.get("Items"))
        .cloned()
        .unwrap_or(Value::Null);
    let mut weapons = Vec::new();
    let Some(map) = items.as_object() else {
        return PlayerLoadout {
            subject,
            character_id,
            weapons,
        };
    };
    for (key, slot) in map {
        let key_lc = key.to_lowercase();
        // `Items` is keyed by weapon UUID; fall back to treating the key as a
        // skin id only if it is not a weapon.
        let weapon = catalog.weapons.get(&key_lc).cloned().or_else(|| {
            catalog.skin_index.get(&key_lc).and_then(|hit| {
                if hit.weapon_uuid.is_empty() {
                    None
                } else {
                    catalog.weapons.get(&hit.weapon_uuid).cloned()
                }
            })
        });
        let Some(weapon) = weapon else { continue };
        let sockets = slot
            .get("Sockets")
            .or_else(|| slot.get("sockets"))
            .and_then(Value::as_object)
            .cloned()
            .unwrap_or_default();
        let mut by_socket: HashMap<String, &Value> = HashMap::new();
        for (sk, sv) in &sockets {
            by_socket.insert(sk.to_lowercase(), sv);
        }
        // Explicit socket reads. The old "first socket wins" fallback is gone:
        // it could grab the buddy socket and paint a buddy uuid as the skin.
        let sock_item_id = |s: Option<&&Value>| -> String {
            let Some(s) = s else { return String::new() };
            let item = s
                .get("Item")
                .or_else(|| s.get("item"))
                .unwrap_or(&Value::Null);
            pick(item, &["ID", "Id", "id", "itemId", "ItemId", "ITEMID"]).to_lowercase()
        };
        let chroma_uuid = sock_item_id(by_socket.get(SKIN_SOCKET));
        let skin_uuid = sock_item_id(by_socket.get(SKIN_ID_SOCKET));
        let level_uuid = sock_item_id(by_socket.get(SKIN_LEVEL_SOCKET));
        let buddy_uuid = sock_item_id(by_socket.get(BUDDY_SOCKET));
        let skin_hit = if skin_uuid.is_empty() {
            None
        } else {
            catalog.skin_index.get(&skin_uuid)
        };
        let chroma_hit = if chroma_uuid.is_empty() {
            None
        } else {
            catalog.skin_index.get(&chroma_uuid)
        };
        // Name: the PARENT skin first. Chroma displayNames are raw internal
        // labels ("Neptune Odin Level 3 / (Variant 1 Black)", sometimes just
        // "Ghost") and must never stand in for the skin name.
        let mut skin_name = weapon.name.clone();
        let mut is_default = true;
        if let Some(hit) = skin_hit {
            if !hit.is_default {
                skin_name = hit.name.clone();
                is_default = false;
            }
        } else if let Some(hit) = chroma_hit {
            if !hit.is_default && !is_bare_weapon_name(&hit.name, &weapon.name) {
                skin_name = hit.name.clone();
                is_default = false;
            }
        }
        // Art: the equipped VARIANT's full render first — chroma displayIcons
        // can be swatches, base-variant renders, or missing, which painted base
        // skins under "Variant X" labels. Then the skin render, then the
        // canonical weapon render (never the grey-X placeholder).
        let chroma_full = chroma_hit
            .filter(|h| !h.is_default)
            .map(|h| h.full_render.clone())
            .unwrap_or_default();
        let icon = if !chroma_full.is_empty() {
            chroma_full
        } else if let Some(hit) = chroma_hit {
            if !hit.is_default && !hit.icon.is_empty() {
                hit.icon.clone()
            } else if let Some(skin) = skin_hit {
                if !skin.is_default && !skin.icon.is_empty() {
                    skin.icon.clone()
                } else {
                    weapon.icon.clone()
                }
            } else {
                weapon.icon.clone()
            }
        } else if let Some(skin) = skin_hit {
            if !skin.is_default && !skin.icon.is_empty() {
                skin.icon.clone()
            } else {
                weapon.icon.clone()
            }
        } else {
            weapon.icon.clone()
        };
        let skin_id = if chroma_uuid.is_empty() {
            skin_uuid.clone()
        } else {
            chroma_uuid.clone()
        };
        let variant_label = catalog
            .chroma_names
            .get(&chroma_uuid)
            .map(|n| variant_label_of(n))
            .unwrap_or_default();
        let level = if level_uuid.is_empty() {
            0
        } else {
            catalog.level_index.get(&level_uuid).copied().unwrap_or(0)
        };
        let (buddy_name, buddy_icon) = catalog
            .buddies
            .get(buddy_uuid.as_str())
            .cloned()
            .unwrap_or_default();
        weapons.push(EquippedWeapon {
            weapon_uuid: weapon.uuid.clone(),
            weapon_name: weapon.name.clone(),
            category: weapon.category.clone(),
            skin_id,
            skin_name,
            icon,
            is_default_skin: is_default,
            variant_label,
            level,
            buddy_name,
            buddy_icon,
        });
    }
    weapons.sort_by(|a, b| {
        order_of(&a.weapon_name)
            .cmp(&order_of(&b.weapon_name))
            .then_with(|| a.weapon_name.cmp(&b.weapon_name))
    });
    PlayerLoadout {
        subject,
        character_id,
        weapons,
    }
}

/// Parse the whole `{ Loadouts: [...] }` payload. A non-array envelope parses
/// to an empty vec: "could not read" renders as the honest empty state, never
/// as invented rows.
pub fn parse_loadouts(raw: &Value, catalog: &Catalog) -> Vec<PlayerLoadout> {
    let list = raw
        .get("Loadouts")
        .or_else(|| raw.get("loadouts"))
        .unwrap_or(raw);
    let Some(list) = list.as_array() else {
        return Vec::new();
    };
    list.iter()
        .map(|e| parse_loadout_entry(e, catalog))
        .collect()
}

/// Pick the loadout belonging to a lobby player.
///
/// Precedence, strongest first:
///   1. `Subject` (PUUID) — exact, immune to ordering changes. Compared
///      dash-insensitively: Riot emits PUUIDs with and without dashes
///      depending on the endpoint, and a strict compare silently misses
///      teammates.
///   2. `CharacterID` — fine in Competitive/Unrated where agents are unique,
///      but Deathmatch and Swiftplay can field duplicates.
///   3. Array position — the payload array is parallel to the match's player
///      list.
///
/// `ambiguous` is set when a weaker key matched several entries, so the caller
/// can disclose that instead of silently showing a stranger's weapons.
pub fn resolve_loadout_for_player<'a>(
    all: &'a [PlayerLoadout],
    puuid: &str,
    character_id: &str,
    index: Option<usize>,
) -> (Option<&'a PlayerLoadout>, bool) {
    fn norm(s: &str) -> String {
        s.trim().to_lowercase().replace('-', "")
    }
    let puuid = norm(puuid);
    if !puuid.is_empty() {
        // Exact-key miss is terminal: falling through to agent or positional
        // matching here can return a DIFFERENT player's loadout with
        // ambiguous=false (unique-agent case). Weaker keys apply only when no
        // puuid was supplied at all.
        let hit = all.iter().find(|l| norm(&l.subject) == puuid);
        return (hit, false);
    }
    let cid = character_id.trim().to_lowercase();
    if !cid.is_empty() {
        let hits: Vec<&PlayerLoadout> = all
            .iter()
            .filter(|l| l.character_id.to_lowercase() == cid)
            .collect();
        if hits.len() == 1 {
            return (Some(hits[0]), false);
        }
        if hits.len() > 1 {
            return (Some(hits[0]), true);
        }
    }
    if let Some(i) = index {
        if let Some(hit) = all.get(i) {
            return (Some(hit), false);
        }
    }
    (None, false)
}

/// One catalogue read from the public content API. Memory-only: the caller
/// keeps the result for the process and never writes it to disk. A failure is
/// an error string, never a partial catalogue that would paint invented rows.
pub async fn fetch_catalog() -> Result<Catalog, String> {
    async fn get(path: &str) -> Result<Value, String> {
        let response = reqwest::Client::builder()
            .timeout(std::time::Duration::from_secs(15))
            .connect_timeout(std::time::Duration::from_secs(5))
            .build()
            .map_err(|_| "Could not reach the weapon catalogue.".to_string())?
            .get(format!("https://valorant-api.com/v1{path}"))
            .header("Accept", "application/json")
            .send()
            .await
            .map_err(|_| "Could not reach the weapon catalogue.".to_string())?;
        if !(200..300).contains(&response.status().as_u16()) {
            return Err(format!(
                "The weapon catalogue refused the read (HTTP {}).",
                response.status()
            ));
        }
        let bytes = response
            .bytes()
            .await
            .map_err(|_| "Unreadable weapon catalogue.".to_string())?;
        let body: Value = serde_json::from_slice(&bytes)
            .map_err(|_| "Unreadable weapon catalogue.".to_string())?;
        Ok(body.get("data").cloned().unwrap_or(Value::Null))
    }
    let (weapons, chromas, buddies) = tokio::try_join!(
        get("/weapons"),
        get("/weapons/skinchromas"),
        get("/buddies")
    )
    .map_err(|e: String| e)?;
    let mut catalog = build_catalog(&weapons, &chromas);
    for b in buddies.as_array().cloned().unwrap_or_default() {
        let Some(uuid) = b.get("uuid").and_then(Value::as_str) else {
            continue;
        };
        catalog.buddies.insert(
            uuid.to_lowercase(),
            (
                b.get("displayName")
                    .and_then(Value::as_str)
                    .unwrap_or("")
                    .to_string(),
                b.get("displayIcon")
                    .and_then(Value::as_str)
                    .unwrap_or("")
                    .to_string(),
            ),
        );
    }
    Ok(catalog)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    const VANDAL_UUID: &str = "9c82e19d-4575-0200-1a81-3eacf00cf872";
    const CLASSIC_UUID: &str = "29a0cfab-485b-f5d5-779a-b59f85e204a8";
    const REAVER_SKIN: &str = "skin-reaver-vandal-uuid";
    const REAVER_CHROMA: &str = "chroma-reaver-v1-uuid";
    const REAVER_LEVEL_4: &str = "level-reaver-4-uuid";
    const BUDDY_UUID: &str = "buddy-fistbump-uuid";

    /// Minimal two-weapon catalogue: canonical art only, no network.
    fn fixture_catalog() -> Catalog {
        let weapons = json!([
            {
                "uuid": VANDAL_UUID,
                "displayName": "Vandal",
                "category": "EEquippableCategory::Rifle",
                "displayIcon": "https://media.valorant-api.com/weapons/vandal/displayicon.png",
                "defaultSkinUuid": "standard-vandal-skin",
                "skins": [
                    {
                        "uuid": "standard-vandal-skin",
                        "displayName": "Standard Vandal",
                        "displayIcon": "https://media.valorant-api.com/weaponskin/grey-x.png",
                        "chromas": [
                            { "uuid": "standard-vandal-chroma", "displayName": "Standard Vandal",
                              "displayIcon": "https://media.valorant-api.com/weaponskin/grey-x.png",
                              "fullRender": "" }
                        ],
                        "levels": [ { "uuid": "standard-vandal-lv1" } ]
                    },
                    {
                        "uuid": REAVER_SKIN,
                        "displayName": "Reaver Vandal",
                        "displayIcon": "https://media.valorant-api.com/weaponskin/reaver-displayicon.png",
                        "chromas": [
                            { "uuid": REAVER_CHROMA,
                              "displayName": "Reaver Vandal Level 4 / (Variant 1 Black)",
                              "displayIcon": "https://media.valorant-api.com/weaponskinchromas/reaver-swatch.png",
                              "fullRender": "https://media.valorant-api.com/weaponskinchromas/reaver-v1-fullrender.png" }
                        ],
                        "levels": [
                            { "uuid": "level-reaver-1-uuid" },
                            { "uuid": "level-reaver-2-uuid" },
                            { "uuid": "level-reaver-3-uuid" },
                            { "uuid": REAVER_LEVEL_4 }
                        ]
                    }
                ]
            },
            {
                "uuid": CLASSIC_UUID,
                "displayName": "Classic",
                "category": "EEquippableCategory::Sidearm",
                "displayIcon": "https://media.valorant-api.com/weapons/classic/displayicon.png",
                "defaultSkinUuid": "standard-classic-skin",
                "skins": [
                    {
                        "uuid": "standard-classic-skin",
                        "displayName": "Standard Classic",
                        "displayIcon": "https://media.valorant-api.com/weaponskin/grey-x.png",
                        "chromas": [],
                        "levels": []
                    }
                ]
            }
        ]);
        let chromas = json!([
            {
                "uuid": REAVER_CHROMA,
                "displayName": "Reaver Vandal Level 4 / (Variant 1 Black)",
                "displayIcon": "https://media.valorant-api.com/weaponskinchromas/reaver-swatch.png",
                "fullRender": "https://media.valorant-api.com/weaponskinchromas/reaver-v1-fullrender.png"
            }
        ]);
        let mut catalog = build_catalog(&weapons, &chromas);
        catalog.buddies.insert(
            BUDDY_UUID.to_string(),
            (
                "Fist Bump".to_string(),
                "https://media.valorant-api.com/buddies/fistbump/displayicon.png".to_string(),
            ),
        );
        catalog
    }

    fn coregame_entry() -> Value {
        json!({
            "CharacterID": "e370fa57-4757-3604-3648-499e1f642d3f",
            "Subject": "11111111-2222-3333-4444-555555555555",
            "Loadout": {
                "Subject": "11111111-2222-3333-4444-555555555555",
                "Items": {
                    VANDAL_UUID: {
                        "ID": VANDAL_UUID,
                        "TypeID": "eequippablecategory::rifle",
                        "Sockets": {
                            SKIN_SOCKET: { "ID": "random-id", "Item": { "ID": REAVER_CHROMA, "TypeID": "skin_chroma" } },
                            SKIN_ID_SOCKET: { "ID": "random-id", "Item": { "ID": REAVER_SKIN, "TypeID": "skin" } },
                            SKIN_LEVEL_SOCKET: { "ID": "random-id", "Item": { "ID": REAVER_LEVEL_4, "TypeID": "skin_level" } },
                            BUDDY_SOCKET: { "ID": "random-id", "Item": { "ID": BUDDY_UUID, "TypeID": "buddy" } }
                        }
                    },
                    CLASSIC_UUID: {
                        "ID": CLASSIC_UUID,
                        "TypeID": "eequippablecategory::sidearm",
                        "Sockets": {
                            SKIN_SOCKET: { "ID": "x", "Item": { "ID": "standard-classic-chroma", "TypeID": "skin_chroma" } },
                            SKIN_ID_SOCKET: { "ID": "x", "Item": { "ID": "standard-classic-skin", "TypeID": "skin" } }
                        }
                    }
                }
            }
        })
    }

    #[test]
    fn a_coregame_entry_parses_the_parent_skin_variant_level_and_buddy() {
        let catalog = fixture_catalog();
        let loadout = parse_loadout_entry(&coregame_entry(), &catalog);
        assert_eq!(loadout.subject, "11111111-2222-3333-4444-555555555555");
        assert_eq!(loadout.weapons.len(), 2);
        // Collection order: Classic (sidearm) before Vandal (rifle).
        assert_eq!(loadout.weapons[0].weapon_name, "Classic");
        assert_eq!(loadout.weapons[1].weapon_name, "Vandal");
        let vandal = &loadout.weapons[1];
        assert_eq!(vandal.skin_name, "Reaver Vandal");
        assert!(!vandal.is_default_skin);
        // Full variant render wins over the swatch displayIcon.
        assert_eq!(
            vandal.icon,
            "https://media.valorant-api.com/weaponskinchromas/reaver-v1-fullrender.png"
        );
        assert_eq!(vandal.variant_label, "Variant 1 Black");
        assert_eq!(vandal.level, 4);
        assert_eq!(vandal.buddy_name, "Fist Bump");
        assert!(!vandal.buddy_icon.is_empty());
    }

    #[test]
    fn a_pregame_entry_without_a_loadout_key_parses_too() {
        // Pregame entries are the loadout object itself — no nesting.
        let catalog = fixture_catalog();
        let entry = json!({
            "Subject": "aaaaaaaa-bbbb-cccc-dddd-eeeeeeeeeeee",
            "CharacterID": "jett-uuid",
            "Items": {
                VANDAL_UUID: {
                    "Sockets": {
                        SKIN_SOCKET: { "Item": { "ID": REAVER_SKIN } },
                        SKIN_ID_SOCKET: { "Item": { "ID": REAVER_SKIN } }
                    }
                }
            }
        });
        let loadout = parse_loadout_entry(&entry, &catalog);
        assert_eq!(loadout.subject, "aaaaaaaa-bbbb-cccc-dddd-eeeeeeeeeeee");
        assert_eq!(loadout.character_id, "jett-uuid");
        assert_eq!(loadout.weapons.len(), 1);
        // A skin uuid in the chroma socket still resolves (both may arrive).
        assert_eq!(loadout.weapons[0].skin_name, "Reaver Vandal");
        assert!(!loadout.weapons[0].is_default_skin);
        // No level socket: 0, not a guessed "Lv 1".
        assert_eq!(loadout.weapons[0].level, 0);
    }

    #[test]
    fn a_default_skin_falls_back_to_canonical_art_never_the_x_placeholder() {
        let catalog = fixture_catalog();
        let loadout = parse_loadout_entry(&coregame_entry(), &catalog);
        let classic = &loadout.weapons[0];
        assert!(classic.is_default_skin);
        assert_eq!(classic.skin_name, "Classic");
        assert_eq!(
            classic.icon,
            "https://media.valorant-api.com/weapons/classic/displayicon.png"
        );
        assert!(
            !classic.icon.contains("grey-x"),
            "a default row must never paint the grey-X placeholder"
        );
    }

    #[test]
    fn an_unknown_chroma_uuid_is_still_a_default_row_with_canonical_art() {
        let catalog = fixture_catalog();
        let entry = json!({
            "Subject": "p1",
            "Loadout": {
                "Subject": "p1",
                "Items": {
                    VANDAL_UUID: {
                        "Sockets": {
                            SKIN_SOCKET: { "Item": { "ID": "never-seen-chroma" } },
                            SKIN_ID_SOCKET: { "Item": { "ID": "never-seen-skin" } }
                        }
                    }
                }
            }
        });
        let loadout = parse_loadout_entry(&entry, &catalog);
        assert_eq!(loadout.weapons.len(), 1);
        assert!(loadout.weapons[0].is_default_skin);
        assert!(!loadout.weapons[0].icon.contains("grey-x"));
    }

    #[test]
    fn an_empty_payload_is_an_honest_empty_state_not_invented_rows() {
        let catalog = fixture_catalog();
        assert!(parse_loadouts(&json!({}), &catalog).is_empty());
        assert!(parse_loadouts(&json!({"Loadouts": []}), &catalog).is_empty());
        assert!(parse_loadouts(&json!("nope"), &catalog).is_empty());
        let entry = parse_loadout_entry(&json!({"Subject": "p1"}), &catalog);
        assert!(entry.weapons.is_empty());
        assert_eq!(entry.subject, "p1");
    }

    #[test]
    fn resolving_prefers_puuid_then_flags_a_duplicate_agent_pick() {
        let catalog = fixture_catalog();
        let raw = json!({
            "Loadouts": [
                {
                    "Subject": "aaaaaaa1-2222-3333-4444-555555555555",
                    "CharacterID": "jett",
                    "Loadout": { "Subject": "aaaaaaa1-2222-3333-4444-555555555555", "Items": {} }
                },
                {
                    "Subject": "bbbbbbb2-2222-3333-4444-555555555555",
                    "CharacterID": "jett",
                    "Loadout": { "Subject": "bbbbbbb2-2222-3333-4444-555555555555", "Items": {} }
                }
            ]
        });
        let all = parse_loadouts(&raw, &catalog);
        assert_eq!(all.len(), 2);
        // Dash-insensitive PUUID match: exact, never ambiguous.
        let (hit, ambiguous) =
            resolve_loadout_for_player(&all, "AAAAAAA1222233334444555555555555", "", None);
        assert_eq!(
            hit.map(|l| l.subject.as_str()),
            Some("aaaaaaa1-2222-3333-4444-555555555555")
        );
        assert!(!ambiguous);
        // No PUUID + a duplicated agent: first entry, flagged ambiguous.
        let (hit, ambiguous) = resolve_loadout_for_player(&all, "", "JETT", None);
        assert!(hit.is_some());
        assert!(ambiguous, "a duplicate agent pick must be disclosed");
        // An unknown PUUID is terminal: no fallback to a stranger's loadout.
        let (hit, ambiguous) = resolve_loadout_for_player(&all, "cccc", "", Some(0));
        assert!(hit.is_none());
        assert!(!ambiguous);
        // Positional fallback only when no PUUID was supplied.
        let (hit, ambiguous) = resolve_loadout_for_player(&all, "", "", Some(1));
        assert_eq!(
            hit.map(|l| l.subject.as_str()),
            Some("bbbbbbb2-2222-3333-4444-555555555555")
        );
        assert!(!ambiguous);
    }

    #[test]
    fn variant_tags_come_from_the_parenthesized_suffix_only() {
        assert_eq!(
            variant_label_of("Reaver Vandal Level 4 / (Variant 1 Black)"),
            "Variant 1 Black"
        );
        assert_eq!(variant_label_of("Reaver Vandal"), "");
        assert_eq!(variant_label_of("Ghost"), "");
        assert_eq!(variant_label_of(""), "");
        assert_eq!(variant_label_of("(nope)"), "");
    }

    #[test]
    fn standard_names_are_defaults_and_real_skins_are_not() {
        assert!(is_default_skin_name("Standard Vandal", "Vandal"));
        assert!(is_default_skin_name("standard vandal", "Vandal"));
        assert!(is_default_skin_name("Vandal", "Vandal"));
        assert!(is_default_skin_name("Melee", "Melee"));
        assert!(!is_default_skin_name("Reaver Vandal", "Vandal"));
        assert!(!is_default_skin_name("Reaver Vandal", ""));
    }

    #[test]
    fn socket_constants_match_the_verified_legacy_values() {
        assert_eq!(SKIN_SOCKET, "3ad1b2b2-acdb-4524-8f52-954a76ddae0a");
        assert_eq!(SKIN_ID_SOCKET, "bcef87d6-209b-46c6-8b19-fbe40bd95abc");
        assert_eq!(SKIN_LEVEL_SOCKET, "e7c63390-eda7-46e0-bb7a-a6abdacd2433");
    }
}
