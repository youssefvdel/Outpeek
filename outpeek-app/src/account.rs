//! The last account Outpeek looked up, persisted so the tracker has a name
//! before — and without — the Riot Client.
//!
//! Why this exists: identity learned from the Riot Client is ephemeral (a
//! lockfile and a loopback socket). The Overview should still open on the last
//! account when Riot is closed, and Tracker Network's public profile API is
//! keyed by the handle, not by anything Riot must be running to supply. So the
//! first successful identity read is written to `%APPDATA%\Outpeek\account.json`
//! and read back at the next launch.
//!
//! Rules:
//! * one file, one shape; a corrupt or partial file reads as "no account",
//!   never as a half-parsed handle;
//! * writes are atomic (temp file + `sync_all` + rename), so a crash mid-write
//!   leaves the previous file intact and no `.tmp` litter;
//! * a handle must be `Name#TAG` with non-empty parts — the shape both the TRN
//!   URL and the player card assume, so it is validated before it is stored.

use std::fs::File;
use std::io::Write as _;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

/// File name under the app data directory.
pub const FILE_NAME: &str = "account.json";
/// The only platform this build reads profiles for.
pub const PLATFORM_RIOT: &str = "riot";
/// Longest handle accepted; Riot's own cap is far below this.
const MAX_HANDLE: usize = 64;
/// Longest tag accepted (Riot's tags are 3–5 today; the reader must not assume).
const MAX_TAG: usize = 16;

/// One persisted account. `saved_at` is epoch milliseconds; the app never
/// deletes the file, it overwrites it when a different account signs in.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Account {
    pub handle: String,
    pub platform: String,
    pub saved_at: u64,
}

impl Account {
    /// Validate first, store normalized (trimmed) values after.
    pub fn new(handle: &str, platform: &str, saved_at: u64) -> Result<Self, String> {
        validate_handle(handle)?;
        let platform = platform.trim();
        Ok(Self {
            handle: handle.trim().to_string(),
            platform: if platform.is_empty() {
                PLATFORM_RIOT.to_string()
            } else {
                platform.to_string()
            },
            saved_at,
        })
    }
}

/// `Name#TAG` → (`Name`, `TAG`). `None` when the shape is wrong; callers must
/// not guess a tag from a bare name.
pub fn split_handle(handle: &str) -> Option<(&str, &str)> {
    let handle = handle.trim();
    let mut parts = handle.split('#');
    let name = parts.next()?.trim();
    let tag = parts.next()?.trim();
    if parts.next().is_some() || name.is_empty() || tag.is_empty() {
        return None;
    }
    Some((name, tag))
}

/// The handle rules, in one place, used by both construction and parsing.
pub fn validate_handle(handle: &str) -> Result<(), String> {
    let handle = handle.trim();
    if handle.is_empty() || handle.len() > MAX_HANDLE {
        return Err("Account handle must be Name#TAG.".to_string());
    }
    // Control characters and URL/router metacharacters are never part of a Riot
    // handle; refusing them here keeps a stored handle safe to percent-encode.
    if handle
        .chars()
        .any(|c| c.is_control() || matches!(c, '?' | '&' | '/' | '\\' | '%'))
    {
        return Err("Account handle has characters Riot handles never use.".to_string());
    }
    let Some((_name, tag)) = split_handle(handle) else {
        return Err("Account handle must be Name#TAG.".to_string());
    };
    if tag.len() > MAX_TAG {
        return Err("Account tag is too long.".to_string());
    }
    if tag.chars().any(char::is_whitespace) {
        return Err("Account tag cannot contain spaces.".to_string());
    }
    Ok(())
}

/// Parse one file's text. A bad shape is an error, so `load` can report
/// "no account" instead of a fabricated default.
pub fn parse(text: &str) -> Result<Account, String> {
    let value: Account =
        serde_json::from_str(text).map_err(|_| "Unreadable account file.".to_string())?;
    let platform = if value.platform.trim().is_empty() {
        PLATFORM_RIOT.to_string()
    } else {
        value.platform.trim().to_string()
    };
    Account::new(&value.handle, &platform, value.saved_at)
}

/// Read the account at `path`. Missing, unreadable, corrupt and invalid all
/// mean the same thing to the caller: there is no remembered account.
pub fn load(path: &Path) -> Option<Account> {
    let text = std::fs::read_to_string(path).ok()?;
    match parse(&text) {
        Ok(account) => Some(account),
        Err(e) => {
            log::warn!("[account] ignoring '{}': {e}", path.display());
            None
        }
    }
}

/// Write the account at `path`, atomically. An invalid account is refused
/// before a byte is written.
pub fn save(path: &Path, account: &Account) -> Result<(), String> {
    let account = Account::new(&account.handle, &account.platform, account.saved_at)?;
    let mut json = serde_json::to_string_pretty(&account)
        .map_err(|_| "Could not encode the account.".to_string())?;
    json.push('\n');
    write_atomic(path, json.as_bytes())
}

/// `%APPDATA%\Outpeek\account.json`, when this host has an app data directory.
pub fn account_path() -> Option<PathBuf> {
    crate::paths::data_file(FILE_NAME)
}

pub fn load_default() -> Option<Account> {
    load(&account_path()?)
}

pub fn save_default(account: &Account) -> Result<(), String> {
    let path = account_path().ok_or_else(|| "No app data directory.".to_string())?;
    save(&path, account)
}

/// Temp file + `sync_all` + rename in the same directory.
///
/// The rename is the commit point: a reader sees the old file or the new one,
/// never a half-written one. On failure the temp file is removed, so a failed
/// save cannot accumulate litter. Public because the TRN profile cache uses the
/// same commit rule.
pub fn write_atomic(path: &Path, bytes: &[u8]) -> Result<(), String> {
    let parent = path
        .parent()
        .ok_or_else(|| "Account path has no directory.".to_string())?;
    std::fs::create_dir_all(parent).map_err(|e| e.to_string())?;
    let tmp = parent.join(temp_name(path));
    let written = (|| -> std::io::Result<()> {
        let mut file = File::create(&tmp)?;
        file.write_all(bytes)?;
        file.sync_all()?;
        drop(file);
        std::fs::rename(&tmp, path)
    })();
    if let Err(e) = written {
        let _ = std::fs::remove_file(&tmp);
        return Err(e.to_string());
    }
    Ok(())
}

/// `.account.json.tmp-<pid>-<nanos>`: unique per process, hidden on Unix, and
/// in the destination directory so the rename stays on one filesystem.
fn temp_name(path: &Path) -> String {
    let base = path
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("account");
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.subsec_nanos())
        .unwrap_or(0);
    format!(".{base}.tmp-{}-{nanos}", std::process::id())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_path(tag: &str) -> (tempfile::TempDir, PathBuf) {
        let dir = tempfile::tempdir().expect("temp dir");
        let path = dir.path().join(FILE_NAME);
        let _ = tag;
        (dir, path)
    }

    fn temp_files(dir: &Path) -> Vec<String> {
        std::fs::read_dir(dir)
            .map(|entries| {
                entries
                    .flatten()
                    .filter_map(|e| e.file_name().to_str().map(str::to_string))
                    .filter(|name| name.contains(".tmp-"))
                    .collect()
            })
            .unwrap_or_default()
    }

    #[test]
    fn a_saved_account_round_trips() {
        let (dir, path) = temp_path("round-trip");
        let account = Account::new("lil ga7ed#zngr", PLATFORM_RIOT, 1_700_000_000_000).unwrap();
        save(&path, &account).unwrap();
        assert_eq!(load(&path), Some(account.clone()));
        // And through the text parser, which is what a hand-edited file hits.
        let text = std::fs::read_to_string(&path).unwrap();
        assert_eq!(parse(&text).unwrap(), account);
        // No temp litter beside the committed file.
        assert!(temp_files(dir.path()).is_empty());
    }

    #[test]
    fn a_corrupt_file_is_no_account_not_a_guess() {
        let (dir, path) = temp_path("corrupt");
        std::fs::write(&path, "{ not json").unwrap();
        assert_eq!(load(&path), None);
        assert!(parse("").is_err());
        assert!(
            parse(r#"{"handle":"lil ga7ed#zngr"}"#).is_err(),
            "missing fields"
        );
        // A JSON body with an invalid handle is refused, not normalized.
        assert!(parse(r#"{"handle":"no-tag-here","platform":"riot","saved_at":1}"#).is_err());
        assert!(temp_files(dir.path()).is_empty());
    }

    #[test]
    fn an_empty_or_malformed_handle_is_refused_before_it_is_stored() {
        for handle in [
            "",
            "   ",
            "lil ga7ed",
            "#zngr",
            "lil ga7ed#",
            "a#b#c",
            "name#ta g",
            "name#tag?x=1",
            "name/tag#zngr",
        ] {
            assert!(
                Account::new(handle, PLATFORM_RIOT, 1).is_err(),
                "handle {handle:?} must be refused"
            );
        }
        assert!(validate_handle("lil ga7ed#zngr").is_ok());
        // The task's rule: a one-character tag is still a shape, not a mistake.
        assert!(validate_handle("name#a").is_ok());
        assert_eq!(
            Account::new("  lil ga7ed#zngr  ", "", 1).unwrap(),
            Account {
                handle: "lil ga7ed#zngr".to_string(),
                platform: PLATFORM_RIOT.to_string(),
                saved_at: 1,
            }
        );
    }

    #[test]
    fn a_failed_write_leaves_the_old_file_and_no_temp_file() {
        let (dir, path) = temp_path("failed-write");
        let first = Account::new("first#tag", PLATFORM_RIOT, 1).unwrap();
        save(&path, &first).unwrap();
        // A directory at the rename target makes the rename fail on every OS.
        let blocked = dir.path().join("blocked.json");
        std::fs::create_dir(&blocked).unwrap();
        let second = Account::new("second#tag", PLATFORM_RIOT, 2).unwrap();
        assert!(save(&blocked, &second).is_err());
        assert!(
            temp_files(dir.path()).is_empty(),
            "temp file must be cleaned up"
        );
        // The earlier file is untouched by the failed write.
        assert_eq!(load(&path), Some(first));
    }

    #[test]
    fn handle_splitting_keeps_spaces_in_the_name() {
        assert_eq!(split_handle("lil ga7ed#zngr"), Some(("lil ga7ed", "zngr")));
        assert_eq!(split_handle(" name # tag "), Some(("name", "tag")));
        assert_eq!(split_handle("name"), None);
        assert_eq!(split_handle("#tag"), None);
        assert_eq!(split_handle("name#"), None);
        assert_eq!(split_handle("a#b#c"), None);
    }
}
