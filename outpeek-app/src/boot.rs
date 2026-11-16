//! Startup timeline, written where a user can find it.
//!
//! The native build ships without a console (`windows_subsystem = windows`), so
//! `eprintln!` goes nowhere on the PC. When "it takes long to open" is the
//! report, the only useful answer is a measured timeline: this module writes one
//! `boot.log` per launch into the app data directory, overwritten each start.
//!
//! Each line is `<milliseconds since process start> <phase>`, so the log is
//! readable as a duration table without parsing timestamps. Failures to write
//! are swallowed: a diagnostic must never be a startup failure.

use std::io::Write;
use std::sync::OnceLock;
use std::time::Instant;

static START: OnceLock<Instant> = OnceLock::new();
static PREPARED: OnceLock<()> = OnceLock::new();

fn start() -> &'static Instant {
    START.get_or_init(Instant::now)
}

fn log_path() -> Option<std::path::PathBuf> {
    crate::paths::app_data_dir().map(|dir| dir.join("boot.log"))
}

/// Mark one phase. The first call truncates the previous launch's file and
/// writes a header, so `boot.log` always describes exactly one start.
pub fn mark(phase: &str) {
    let elapsed = start().elapsed().as_secs_f64() * 1000.0;
    eprintln!("[boot] {elapsed:7.1}ms {phase}");
    PREPARED.get_or_init(|| {
        if let Some(path) = log_path() {
            let header = format!(
                "Outpeek {} · {} · {}\n",
                crate::VERSION,
                if cfg!(debug_assertions) {
                    "debug"
                } else {
                    "release"
                },
                env!("OUTPEEK_TARGET")
            );
            let _ = std::fs::write(&path, header);
        }
        ()
    });
    if let Some(path) = log_path() {
        if let Ok(mut file) = std::fs::OpenOptions::new().append(true).open(&path) {
            let _ = writeln!(file, "{elapsed:7.1}ms {phase}");
        }
    }
}
