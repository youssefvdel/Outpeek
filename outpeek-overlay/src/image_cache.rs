//! URL -> disk image cache for agent icons / rank crests.
//!
//! Ownership split: the MAIN app downloads (it owns the remote HTTP transport)
//! and writes response bytes through [`store_bytes_in`]; every reader only
//! loads local files through [`load_from`]. Files are content-addressed by URL
//! hash so a stale portrait can never serve a new agent, and a directory is
//! capped with mtime LRU eviction ([`evict_lru_in`]).
//!
//! Two callers own two different directories, so every file operation takes the
//! directory explicitly: the app's art cache lives under `%APPDATA%\Outpeek\
//! cache\art` (it owns the TTL policy), while the standalone HUD sidecar keeps
//! its own directory via [`cache_dir`]. One implementation, no copies.
//!
//! No Riot asset is committed to the repo: these files are downloaded at
//! runtime into the user's own app-data directory.

use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use slint::{Image, Rgb8Pixel, Rgba8Pixel, SharedPixelBuffer};

/// Budget for the sidecar's portrait/crest cache (~50 MB). The app's art cache
/// carries its own budget (`outpeek_app::assets::CACHE_MAX_BYTES`).
pub const CACHE_BUDGET_BYTES: u64 = 50 * 1024 * 1024;

/// Sidecar cache directory. The app never uses this one.
pub fn cache_dir() -> PathBuf {
    #[cfg(windows)]
    {
        std::env::var("LOCALAPPDATA")
            .map(PathBuf::from)
            .unwrap_or_else(|_| PathBuf::from(r"C:\Users\Administrator\AppData\Local"))
            .join("Outpeek")
            .join("overlay-images")
    }
    #[cfg(not(windows))]
    {
        std::env::var("XDG_CACHE_HOME")
            .map(PathBuf::from)
            .unwrap_or_else(|_| {
                std::env::var("HOME")
                    .map(|h| PathBuf::from(h).join(".cache"))
                    .unwrap_or_else(|_| std::env::temp_dir())
            })
            .join("outpeek-overlay-images")
    }
}

/// Stable filename for a URL inside `dir`: FNV-1a hash + the origin extension
/// when it looks like an image suffix, `.bin` otherwise (Slint/`image` sniff
/// content, not names).
pub fn path_for_url_in(dir: &Path, url: &str) -> PathBuf {
    let mut hash: u64 = 0xcbf29ce484222325;
    for b in url.bytes() {
        hash ^= b as u64;
        hash = hash.wrapping_mul(0x100000001b3);
    }
    let ext = url
        .split(['?', '#'])
        .next()
        .unwrap_or("")
        .rsplit('.')
        .next()
        .map(str::to_lowercase)
        .filter(|e| matches!(e.as_str(), "png" | "jpg" | "jpeg" | "webp" | "gif"))
        .unwrap_or_else(|| "bin".to_string());
    dir.join(format!("{hash:016x}.{ext}"))
}

/// [`path_for_url_in`] in the sidecar's own directory.
pub fn path_for_url(url: &str) -> PathBuf {
    path_for_url_in(&cache_dir(), url)
}

/// Write downloaded bytes for a URL into `dir`. Returns the file path.
///
/// Atomic by construction: the bytes go to a hidden sibling and are renamed
/// over the final name. A crash or a full disk mid-write therefore leaves the
/// previous copy intact instead of a half-PNG that [`load_from`] can never
/// decode but the TTL will not refresh until it expires.
pub fn store_bytes_in(dir: &Path, url: &str, bytes: &[u8]) -> std::io::Result<PathBuf> {
    std::fs::create_dir_all(dir)?;
    let path = path_for_url_in(dir, url);
    // Unique per call: two threads writing the same URL must not share a temp
    // file, or one rename lands on a truncated body.
    static SEQ: AtomicU64 = AtomicU64::new(0);
    let seq = SEQ.fetch_add(1, Ordering::Relaxed);
    let tmp = dir.join(format!(".{seq}-{}.tmp", std::process::id()));
    // Any failure past this point must not leave the temp file behind.
    let written = (|| -> std::io::Result<()> {
        let mut file = std::fs::File::create(&tmp)?;
        file.write_all(bytes)?;
        file.sync_all()?;
        Ok(())
    })();
    if let Err(e) = written {
        let _ = std::fs::remove_file(&tmp);
        return Err(e);
    }
    if let Err(e) = std::fs::rename(&tmp, &path) {
        let _ = std::fs::remove_file(&tmp);
        return Err(e);
    }
    Ok(path)
}

/// [`store_bytes_in`] in the sidecar's own directory.
pub fn store_bytes(url: &str, bytes: &[u8]) -> std::io::Result<PathBuf> {
    let path = store_bytes_in(&cache_dir(), url, bytes)?;
    evict_lru(CACHE_BUDGET_BYTES);
    Ok(path)
}

/// Decode one cached file as a Slint image. Missing/invalid files are NOT
/// errors: the caller renders its drawn fallback (the HUD leaves unknown icons
/// empty; never placeholders-as-data).
pub fn load_from(dir: &Path, url: &str) -> Image {
    if url.is_empty() {
        return Image::default();
    }
    load_path(&path_for_url_in(dir, url))
}

/// [`load_from`] in the sidecar's own directory.
pub fn load(url: &str) -> Image {
    load_from(&cache_dir(), url)
}

/// Decode a local image file for Slint.
///
/// Riot's art is PNG with real alpha (rank crests sit on variables backgrounds,
/// agent portraits are cut-outs), so this keeps the alpha channel and
/// premultiplies it — `Image::from_rgba8` requires premultiplied pixels, and
/// feeding it straight alpha renders every cut-out with a dark fringe.
pub fn load_path(path: &Path) -> Image {
    // Sniff content, not the filename: hash names may carry `.bin`.
    let Ok(bytes) = std::fs::read(path) else {
        return Image::default();
    };
    let Ok(dyn_img) = image::load_from_memory(&bytes) else {
        return Image::default();
    };
    let rgba = dyn_img.to_rgba8();
    let (w, h) = (rgba.width(), rgba.height());
    if w == 0 || h == 0 || w > 1024 || h > 1024 {
        return Image::default();
    }
    if rgba.pixels().all(|p| p.0[3] == u8::MAX) {
        // Fully opaque: RGB is enough and saves a third of the buffer.
        let mut buf: SharedPixelBuffer<Rgb8Pixel> = SharedPixelBuffer::new(w, h);
        for (dst, src) in buf
            .make_mut_bytes()
            .chunks_exact_mut(3)
            .zip(rgba.as_raw().chunks_exact(4))
        {
            dst.copy_from_slice(&src[..3]);
        }
        return Image::from_rgb8(buf);
    }
    let mut buf: SharedPixelBuffer<Rgba8Pixel> = SharedPixelBuffer::new(w, h);
    for (dst, src) in buf
        .make_mut_bytes()
        .chunks_exact_mut(4)
        .zip(rgba.as_raw().chunks_exact(4))
    {
        let a = src[3] as u32;
        // Premultiply in u32 then round back to u8.
        for c in 0..3 {
            dst[c] = ((src[c] as u32 * a + 127) / 255) as u8;
        }
        dst[3] = src[3];
    }
    Image::from_rgba8(buf)
}

/// Every regular file directly inside `dir`: (mtime, bytes, path).
fn files_in(dir: &Path) -> Vec<(std::time::SystemTime, u64, PathBuf)> {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return Vec::new();
    };
    entries
        .filter_map(|e| e.ok())
        .filter_map(|e| {
            let meta = e.metadata().ok()?;
            if !meta.is_file() {
                return None;
            }
            Some((meta.modified().ok()?, meta.len(), e.path()))
        })
        .collect()
}

/// `(file count, total bytes)` on disk inside `dir`.
pub fn usage_in(dir: &Path) -> (usize, u64) {
    let files = files_in(dir);
    (files.len(), files.iter().map(|(_, len, _)| *len).sum())
}

/// [`usage_in`] for the sidecar's own directory.
pub fn usage() -> (usize, u64) {
    usage_in(&cache_dir())
}

/// Drop oldest files (by mtime) in `dir` until it fits `max_bytes`. Returns the
/// number of bytes freed, so the caller can log the eviction instead of
/// assuming it happened.
pub fn evict_lru_in(dir: &Path, max_bytes: u64) -> u64 {
    let mut files = files_in(dir);
    let total: u64 = files.iter().map(|(_, len, _)| *len).sum();
    if total <= max_bytes {
        return 0;
    }
    files.sort_by_key(|(mtime, _, _)| *mtime);
    let mut freed = 0u64;
    let need = total - max_bytes;
    for (_, len, path) in files {
        if freed >= need {
            break;
        }
        if std::fs::remove_file(&path).is_ok() {
            freed += len;
        }
    }
    freed
}

/// [`evict_lru_in`] in the sidecar's own directory.
pub fn evict_lru(max_bytes: u64) -> u64 {
    evict_lru_in(&cache_dir(), max_bytes)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A throwaway directory per test, removed on drop.
    struct TmpDir(PathBuf);
    impl TmpDir {
        fn new(tag: &str) -> Self {
            static SEQ: AtomicU64 = AtomicU64::new(0);
            let n = SEQ.fetch_add(1, Ordering::Relaxed);
            let dir = std::env::temp_dir().join(format!(
                "outpeek-image-cache-{tag}-{}-{n}",
                std::process::id()
            ));
            let _ = std::fs::remove_dir_all(&dir);
            std::fs::create_dir_all(&dir).unwrap();
            Self(dir)
        }
        fn path(&self) -> &Path {
            &self.0
        }
    }
    impl Drop for TmpDir {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    fn png_with_alpha(alpha: u8) -> Vec<u8> {
        use image::ImageEncoder;
        let mut png: Vec<u8> = Vec::new();
        let img = image::RgbaImage::from_fn(4, 4, |x, y| {
            image::Rgba([(x * 64) as u8, (y * 64) as u8, 128, alpha])
        });
        image::codecs::png::PngEncoder::new(&mut png)
            .write_image(img.as_raw(), 4, 4, image::ExtendedColorType::Rgba8)
            .unwrap();
        png
    }

    #[test]
    fn url_paths_are_stable_and_suffixed() {
        let dir = Path::new("/tmp/art");
        let a = path_for_url_in(dir, "https://example.com/Jett.png?w=1");
        let b = path_for_url_in(dir, "https://example.com/Jett.png?w=1");
        let c = path_for_url_in(dir, "https://example.com/Omen.png?w=1");
        assert_eq!(a, b);
        assert_ne!(a, c);
        assert_eq!(a.extension().and_then(|e| e.to_str()), Some("png"));
        // The file lands in the requested directory, not a global one.
        assert_eq!(a.parent(), Some(dir));
        // A query string cannot smuggle a different extension in.
        let noext = path_for_url_in(dir, "https://example.com/art?id=x.png");
        assert_eq!(noext.extension().and_then(|e| e.to_str()), Some("bin"));
    }

    #[test]
    fn store_and_load_roundtrip() {
        let dir = TmpDir::new("roundtrip");
        let png = png_with_alpha(u8::MAX);
        let url = "https://example.com/roundtrip-agent.png";
        let path = store_bytes_in(dir.path(), url, &png).unwrap();
        assert!(path.exists());
        let loaded = load_from(dir.path(), url);
        assert_eq!((loaded.size().width, loaded.size().height), (4, 4));
    }

    #[test]
    fn store_leaves_no_temp_file_behind() {
        let dir = TmpDir::new("atomic");
        let url = "https://example.com/atomic.png";
        store_bytes_in(dir.path(), url, &png_with_alpha(u8::MAX)).unwrap();
        // Overwriting a cached file must also leave exactly one file: a
        // leftover `.tmp` would count against the size budget forever.
        store_bytes_in(dir.path(), url, &png_with_alpha(u8::MAX)).unwrap();
        let names: Vec<String> = std::fs::read_dir(dir.path())
            .unwrap()
            .filter_map(|e| e.ok())
            .map(|e| e.file_name().to_string_lossy().to_string())
            .collect();
        assert_eq!(names.len(), 1, "expected one file, found {names:?}");
        assert!(!names[0].starts_with('.'), "{names:?}");
        // The bytes on disk are the new ones, whole.
        let on_disk = std::fs::read(path_for_url_in(dir.path(), url)).unwrap();
        assert_eq!(on_disk, png_with_alpha(u8::MAX));
    }

    #[test]
    fn a_transparent_png_keeps_its_alpha_channel() {
        let dir = TmpDir::new("alpha");
        let url = "https://example.com/crest-alpha.png";
        store_bytes_in(dir.path(), url, &png_with_alpha(0)).unwrap();
        let image = load_from(dir.path(), url);
        assert_eq!(image.size().width, 4);
        // Fully transparent pixels must stay transparent: an RGB-only decode
        // renders them black, which is a dark square behind every crest.
        let rgba = image.to_rgba8().expect("decoded as rgba");
        assert!(
            rgba.as_bytes().chunks_exact(4).all(|p| p[3] == 0),
            "alpha was dropped"
        );
        // Half-transparent art keeps its alpha too (premultiplied on the way
        // in, un-premultiplied by this read).
        let half = "https://example.com/crest-half.png";
        store_bytes_in(dir.path(), half, &png_with_alpha(128)).unwrap();
        let rgba = load_from(dir.path(), half).to_rgba8().expect("rgba");
        assert!(
            rgba.as_bytes().chunks_exact(4).all(|p| p[3] == 128),
            "alpha {:#?}",
            rgba.as_bytes()
                .chunks_exact(4)
                .map(|p| p[3])
                .collect::<Vec<_>>()
        );
    }

    #[test]
    fn load_missing_is_empty_not_error() {
        let dir = TmpDir::new("missing");
        let img = load_from(dir.path(), "https://example.com/never-cached-0000.png");
        assert_eq!(img.size().width, 0);
        // An empty URL is empty too, and never touches the filesystem.
        assert_eq!(load_from(dir.path(), "").size().width, 0);
        assert_eq!(load("").size().width, 0);
    }

    #[test]
    fn a_corrupt_file_loads_as_empty() {
        let dir = TmpDir::new("corrupt");
        let url = "https://example.com/corrupt.png";
        store_bytes_in(dir.path(), url, b"<html>not a png</html>").unwrap();
        assert_eq!(load_from(dir.path(), url).size().width, 0);
    }

    #[test]
    fn evict_lru_keeps_newest_under_budget() {
        let dir = TmpDir::new("evict");
        let now = std::time::SystemTime::now();
        for i in 0..4u8 {
            let p = dir.path().join(format!("evict-test-{i}.bin"));
            std::fs::write(&p, vec![i; 1024]).unwrap();
            // Stagger mtimes: file 0 oldest, file 3 newest.
            let t = now - std::time::Duration::from_secs((4 - i as u64) * 10);
            let ft = filetime::FileTime::from_system_time(t);
            filetime::set_file_mtime(&p, ft).unwrap();
        }
        let freed = evict_lru_in(dir.path(), 2500);
        let remaining: Vec<_> = std::fs::read_dir(dir.path())
            .unwrap()
            .filter_map(|e| e.ok())
            .filter(|e| {
                e.file_name()
                    .to_str()
                    .map(|n| n.starts_with("evict-test-"))
                    .unwrap_or(false)
            })
            .collect();
        // 4x1024 = 4096 > 2500: the two oldest must go, the two newest stay.
        assert_eq!(remaining.len(), 2, "evict kept {remaining:?}");
        assert_eq!(freed, 2048);
        for e in &remaining {
            let name = e.file_name().to_string_lossy().to_string();
            // The two newest survive; the two oldest are gone.
            assert!(
                name == "evict-test-2.bin" || name == "evict-test-3.bin",
                "unexpected survivor {name}"
            );
        }
    }

    #[test]
    fn evicting_an_empty_or_small_dir_frees_nothing() {
        let dir = TmpDir::new("evict-noop");
        assert_eq!(evict_lru_in(dir.path(), 1024), 0);
        assert_eq!(usage_in(dir.path()), (0, 0));
        std::fs::write(dir.path().join("a.bin"), vec![0u8; 100]).unwrap();
        assert_eq!(evict_lru_in(dir.path(), 1024), 0);
        assert_eq!(usage_in(dir.path()), (1, 100));
    }
}
