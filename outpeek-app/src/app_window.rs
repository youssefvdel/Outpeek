//! App-window chrome for the frameless shell.
//!
//! The window has no system title bar (`no-frame: true` in `ui/app.slint`) and
//! `ui/components/titlebar.slint` draws our own. That is not a style choice: on
//! the real machine a contiguous band of DWM's title-bar surface sat above the
//! client area, and `DWMWA_CAPTION_COLOR` only repainted it (red still showed
//! the band). A window without a caption has no title-bar surface to paint.
//!
//! This module owns the Win32 half — window commands, the maximised fact, and
//! the rounded-corner clip — and the geometry contract the titlebar implements.
//! Everything that can be pure is pure, so the host tests it; only the Win32
//! calls are behind `cfg(windows)`.
//!
//! Not the HUD: `outpeek_overlay::win_style` owns that window's recipe
//! (DONOTROUND, transparent, click-through). A rounded captioned frame on the
//! HUD is the bug that recipe exists to prevent.

use outpeek_overlay::roles::WindowRole;

// ---------------------------------------------------------------------------
// Geometry — one set of numbers for the drawn titlebar and the Win32 maths.
// `main.rs` pushes TITLEBAR_HEIGHT and CONTROL_WIDTH into `Tracker` at boot, so
// the Slint layout and the hit regions below are the same values, not copies.
// ---------------------------------------------------------------------------

/// Titlebar height in logical pixels. Fits the 14 px wordmark with air on both
/// sides and still reads as chrome, not as a second header.
pub const TITLEBAR_HEIGHT: f32 = 38.0;
/// One window button's width. 46 px is the Windows caption-button width at
/// 100 %: wide enough to hit, narrow enough to leave a drag surface.
pub const CONTROL_WIDTH: f32 = 46.0;
/// Minimise, maximise/restore, close.
pub const CONTROL_COUNT: usize = 3;
/// `Shape.lg` (16 px logical) — the window's corner radius from the token set.
pub const WINDOW_RADIUS: f32 = 16.0;

/// Axis-aligned rectangle in logical pixels, origin at the window's top left.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Rect {
    pub x: f32,
    pub y: f32,
    pub width: f32,
    pub height: f32,
}

impl Rect {
    /// Half-open in x and y: `[x, x+width)`, so two adjacent regions never
    /// both claim a pixel.
    pub fn contains(&self, px: f32, py: f32) -> bool {
        px >= self.x && px < self.x + self.width && py >= self.y && py < self.y + self.height
    }
}

/// The titlebar's two kinds of target, as `ui/components/titlebar.slint`
/// arranges them.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct TitlebarLayout {
    /// Everything left of the buttons: `WindowMoveArea` in the UI.
    pub drag: Rect,
    /// Minimise, maximise/restore, close — contiguous, flush to the right edge.
    pub controls: [Rect; CONTROL_COUNT],
}

/// Control index in draw order (also the order the shell declares them).
pub const MINIMIZE: usize = 0;
pub const MAXIMIZE: usize = 1;
pub const CLOSE: usize = 2;

/// The layout for a window `window_width` logical pixels wide.
///
/// A drag of the buttons would be a missed click; a press on the drag surface
/// would be a window move. Keeping both in one function is what makes "the
/// drag area excludes the buttons" checkable instead of assumed.
pub fn titlebar_layout(window_width: f32) -> TitlebarLayout {
    let controls_width = CONTROL_WIDTH * CONTROL_COUNT as f32;
    // Never negative: at a window narrower than the three buttons the drag
    // surface collapses to zero instead of wrapping behind them.
    let drag_width = (window_width - controls_width).max(0.0);
    TitlebarLayout {
        drag: Rect {
            x: 0.0,
            y: 0.0,
            width: drag_width,
            height: TITLEBAR_HEIGHT,
        },
        controls: std::array::from_fn(|i| Rect {
            x: window_width - controls_width + CONTROL_WIDTH * i as f32,
            y: 0.0,
            width: CONTROL_WIDTH,
            height: TITLEBAR_HEIGHT,
        }),
    }
}

/// Where a titlebar press at (`x`, `y`) lands.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TitlebarHit {
    Drag,
    Control(usize),
    Outside,
}

pub fn titlebar_hit(layout: &TitlebarLayout, x: f32, y: f32) -> TitlebarHit {
    for (i, control) in layout.controls.iter().enumerate() {
        if control.contains(x, y) {
            return TitlebarHit::Control(i);
        }
    }
    if layout.drag.contains(x, y) {
        TitlebarHit::Drag
    } else {
        TitlebarHit::Outside
    }
}

// ---------------------------------------------------------------------------
// Window commands
// ---------------------------------------------------------------------------

/// What the titlebar can ask the window system to do.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WindowCommand {
    Minimize,
    /// Maximise when normal, restore when maximised — one button, as on Windows.
    ToggleMaximize,
    Close,
}

/// What actually happened, in terms the caller can log. Never a guess: every
/// command reads the window state back instead of trusting a BOOL.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CommandReport {
    pub command: WindowCommand,
    pub applied: bool,
    pub detail: &'static str,
}

impl CommandReport {
    pub fn log_line(&self) -> String {
        format!(
            "app-window command {:?}: {} ({})",
            self.command,
            if self.applied { "ok" } else { "refused" },
            self.detail
        )
    }
}

/// The maximise button's glyph and accessible label for a window state.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MaxControl {
    pub glyph: &'static str,
    pub label: &'static str,
}

/// Rust owns this mapping: the UI renders `glyph` and never decides what the
/// button means. A maximised window's button restores it.
pub fn max_control(maximized: bool) -> MaxControl {
    if maximized {
        MaxControl {
            glyph: "restore",
            label: "Restore",
        }
    } else {
        MaxControl {
            glyph: "maximize",
            label: "Maximize",
        }
    }
}

// ---------------------------------------------------------------------------
// Rounded corners
// ---------------------------------------------------------------------------

/// `DWMWCP_DONOTROUND` — square corners (the HUD's requirement).
pub const DWMWCP_DONOTROUND: u32 = 1;
/// `DWMWCP_ROUND` — a small radius on all four corners.
pub const DWMWCP_ROUND: u32 = 2;
/// `DWMWA_COLOR_NONE` — DWM draws no border line at all.
pub const DWMWA_COLOR_NONE: u32 = 0xFFFF_FFFE;

/// DWM attribute ids. Named because raw numbers are unreadable at the call
/// site and a typo would silently colour the wrong part of the frame.
pub const DWMWA_USE_IMMERSIVE_DARK_MODE: u32 = 20;
pub const DWMWA_WINDOW_CORNER_PREFERENCE: u32 = 33;
pub const DWMWA_BORDER_COLOR: u32 = 34;

/// Which corner preference a window role gets.
///
/// App: rounded, because it is a normal application window.
/// Overlay: never rounded — its recipe already asks DWM for square corners and
/// the HUD's pixel contract assumes the full rectangle.
pub fn corner_preference(role: WindowRole) -> u32 {
    match role {
        WindowRole::App => DWMWCP_ROUND,
        WindowRole::Overlay => DWMWCP_DONOTROUND,
    }
}

/// How the app window's corners are produced, decided by the HRESULT DWM
/// returned for `DWMWA_WINDOW_CORNER_PREFERENCE`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CornerMode {
    /// DWM paints the rounding, anti-aliased. No region is set: a region would
    /// blunt that edge. (A frameless winit window has no DWM drop shadow
    /// either way — winit's `decoration_shadow` is off and Slint does not
    /// expose it — so nothing is lost by not clipping.)
    Dwm,
    /// DWM refused the attribute, so the window is clipped with a region.
    /// Square-ish edges, but the corners never lie about being rounded.
    Region,
}

/// winit keeps `WS_CAPTION | WS_THICKFRAME` on a frameless window — it only
/// overrides the non-client area in `WM_NCCALCSIZE` — so DWM sees a normally
/// framed window and `DWMWCP_ROUND` applies. That is why DWM is the first
/// choice; a refused HRESULT is what switches to the region fallback.
pub fn corner_mode(corner_hr: i32) -> CornerMode {
    if corner_hr >= 0 {
        CornerMode::Dwm
    } else {
        CornerMode::Region
    }
}

/// The one line that proves which corner path the window got.
pub fn corner_log(mode: CornerMode, applied: Option<Region>, scale_factor: f32) -> String {
    match (mode, applied) {
        (CornerMode::Dwm, _) => {
            format!("app-window corners: DWM ROUND (no clip) scale={scale_factor:.2}")
        }
        (CornerMode::Region, Some(region)) => format!(
            "app-window corners: window region {}x{} r={} (ellipse {}) scale={scale_factor:.2}",
            region.width,
            region.height,
            region.radius,
            region_ellipse(region.radius)
        ),
        (CornerMode::Region, None) => format!(
            "app-window corners: region mode, currently unclipped (maximised or refused) \
             scale={scale_factor:.2}"
        ),
    }
}

/// A window region, in physical pixels (Win32 has no other unit here).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Region {
    pub width: i32,
    pub height: i32,
    /// Corner radius in physical pixels.
    pub radius: i32,
}

/// `Shape.lg` scaled from logical to physical pixels for this monitor.
pub fn region_radius(scale_factor: f32) -> i32 {
    (WINDOW_RADIUS * scale_factor.max(0.0)).round() as i32
}

/// `CreateRoundRectRgn` takes the *width and height of the ellipse*, i.e. twice
/// the corner radius. Passing the radius straight through halves every corner —
/// a bug that looks like the wrong token, not like the wrong API.
pub fn region_ellipse(radius: i32) -> i32 {
    (radius * 2).max(0)
}

/// The clip the window needs right now, or `None` when it should have none.
pub fn wanted_region(
    mode: CornerMode,
    maximized: bool,
    width: i32,
    height: i32,
    scale_factor: f32,
) -> Option<Region> {
    match mode {
        // DWM rounds it: a region would only blunt the anti-aliased edge and
        // drop the system shadow.
        CornerMode::Dwm => None,
        // A maximised window owns the whole work area; clipping its corners
        // would show the desktop through them.
        CornerMode::Region if maximized => None,
        CornerMode::Region if width <= 0 || height <= 0 => None,
        CornerMode::Region => Some(Region {
            width,
            height,
            radius: region_radius(scale_factor),
        }),
    }
}

/// Whether the window needs a new region now.
///
/// Three cases: nothing to do when the clip is already right; apply when the
/// clip changed; and do *not* retry a clip Windows already refused at this
/// size — a refused region is a cosmetic miss, not a reason to rebuild the
/// window shape (and log) every poll tick.
pub fn should_apply_region(
    applied: Option<Region>,
    wanted: Option<Region>,
    refused: Option<Region>,
) -> bool {
    if applied == wanted {
        return false;
    }
    !(wanted.is_some() && refused == wanted)
}

// ---------------------------------------------------------------------------
// Win32
// ---------------------------------------------------------------------------

// Win32 constants for the titlebar commands: `SW_MINIMIZE`, `SW_MAXIMIZE`,
// `SW_RESTORE` (ShowWindow) and `WM_CLOSE` (PostMessage). Named because a bare
// 6/3/9 at a call site is unreadable and a swap would minimise instead of
// restore.
#[cfg(windows)]
const SW_MINIMIZE: i32 = 6;
#[cfg(windows)]
const SW_MAXIMIZE: i32 = 3;
#[cfg(windows)]
const SW_RESTORE: i32 = 9;
#[cfg(windows)]
const WM_CLOSE: u32 = 0x0010;

/// One HRESULT per attribute, kept so startup can log what Windows accepted
/// instead of claiming the styling worked.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct StyleReport {
    pub corner: i32,
    pub border: i32,
    pub dark_mode: i32,
}

impl StyleReport {
    /// Windows refused an attribute if the HRESULT is negative (`FAILED`).
    /// A refused attribute is a cosmetic miss, never a reason to panic.
    pub fn all_ok(&self) -> bool {
        [self.corner, self.border, self.dark_mode]
            .iter()
            .all(|hr| *hr >= 0)
    }

    /// The one startup line that proves what DWM accepted for the frameless
    /// window. There is deliberately no caption colour: a window with no
    /// caption has no caption to paint.
    pub fn log_line(&self) -> String {
        format!(
            "app-window chrome (frameless): corner=ROUND hr=0x{:08X} border=NONE hr=0x{:08X} \
             dark-mode hr=0x{:08X}",
            self.corner as u32, self.border as u32, self.dark_mode as u32,
        )
    }
}

/// Apply the app-window attributes and report each HRESULT.
///
/// Never panics: a refused attribute is returned in the report for the caller
/// to log, and [`corner_mode`] turns a refused corner into the region fallback.
#[cfg(windows)]
pub fn style_app_window(hwnd: isize) -> StyleReport {
    use std::os::raw::c_void;

    #[link(name = "dwmapi")]
    extern "system" {
        // HWND is a pointer-sized handle; `isize` keeps this module free of the
        // windows-rs types the overlay crate needs for its full recipe.
        fn DwmSetWindowAttribute(
            hwnd: isize,
            attribute: u32,
            value: *const c_void,
            size: u32,
        ) -> i32;
    }

    let set = |attribute: u32, value: u32| -> i32 {
        let size = std::mem::size_of::<u32>() as u32;
        // SAFETY: `value` lives for the whole call and DWM only reads it.
        unsafe {
            DwmSetWindowAttribute(hwnd, attribute, &value as *const u32 as *const c_void, size)
        }
    };

    StyleReport {
        corner: set(
            DWMWA_WINDOW_CORNER_PREFERENCE,
            corner_preference(WindowRole::App),
        ),
        // No accent line around the client area.
        border: set(DWMWA_BORDER_COLOR, DWMWA_COLOR_NONE),
        // Keep system menus and the shadow dark on the dark surface.
        dark_mode: set(DWMWA_USE_IMMERSIVE_DARK_MODE, 1),
    }
}

#[cfg(windows)]
extern "system" {
    fn ShowWindow(hwnd: isize, cmd: i32) -> i32;
    fn IsZoomed(hwnd: isize) -> i32;
    fn IsIconic(hwnd: isize) -> i32;
    fn PostMessageW(hwnd: isize, msg: u32, wparam: usize, lparam: isize) -> i32;
    fn SetWindowRgn(hwnd: isize, rgn: isize, redraw: i32) -> i32;
}

#[cfg(windows)]
#[link(name = "gdi32")]
extern "system" {
    fn CreateRoundRectRgn(
        x1: i32,
        y1: i32,
        x2: i32,
        y2: i32,
        ellipse_w: i32,
        ellipse_h: i32,
    ) -> isize;
    fn DeleteObject(object: isize) -> i32;
}

/// True when the window is maximised, read from the window, not remembered.
#[cfg(windows)]
pub fn is_maximized(hwnd: isize) -> bool {
    // SAFETY: the caller passes the HWND Slint just handed us.
    unsafe { IsZoomed(hwnd) != 0 }
}

/// Run one titlebar command and report the state it produced.
#[cfg(windows)]
pub fn apply_window_command(hwnd: isize, command: WindowCommand) -> CommandReport {
    match command {
        WindowCommand::Minimize => {
            // SAFETY: HWND from the live Slint window.
            unsafe { ShowWindow(hwnd, SW_MINIMIZE) };
            // ShowWindow's BOOL is the *previous* visibility, not success;
            // read the state back instead of trusting it.
            // SAFETY: as above.
            let applied = unsafe { IsIconic(hwnd) } != 0;
            CommandReport {
                command,
                applied,
                detail: if applied { "IsIconic" } else { "still visible" },
            }
        }
        WindowCommand::ToggleMaximize => {
            // SAFETY: HWND from the live Slint window.
            let was_maximized = unsafe { IsZoomed(hwnd) } != 0;
            // SAFETY: as above.
            unsafe {
                ShowWindow(
                    hwnd,
                    if was_maximized {
                        SW_RESTORE
                    } else {
                        SW_MAXIMIZE
                    },
                );
            }
            // SAFETY: as above.
            let now = unsafe { IsZoomed(hwnd) } != 0;
            CommandReport {
                command,
                applied: now != was_maximized,
                detail: if now { "IsZoomed" } else { "normal" },
            }
        }
        WindowCommand::Close => {
            // The close goes through WM_CLOSE so it takes the same path as the
            // system close button (Slint's close-requested handler quits).
            // SAFETY: HWND from the live Slint window.
            let applied = unsafe { PostMessageW(hwnd, WM_CLOSE, 0, 0) } != 0;
            CommandReport {
                command,
                applied,
                detail: "WM_CLOSE posted",
            }
        }
    }
}

/// Put `wanted` on the window as its region, or clear the region when `None`.
///
/// Returns the `SetWindowRgn` BOOL: non-zero means Windows owns the region and
/// applied it. On failure Windows does not take ownership, so the region is
/// deleted here rather than leaked for the life of the process.
#[cfg(windows)]
pub fn apply_region(hwnd: isize, wanted: Option<Region>) -> bool {
    match wanted {
        // SAFETY: HWND from the live Slint window; a null HRGN removes the
        // region and `TRUE` repaints the frame.
        None => (unsafe { SetWindowRgn(hwnd, 0, 1) }) != 0,
        Some(region) => {
            // SAFETY: plain GDI call with integer geometry.
            let rgn = unsafe {
                CreateRoundRectRgn(
                    0,
                    0,
                    region.width,
                    region.height,
                    region_ellipse(region.radius),
                    region_ellipse(region.radius),
                )
            };
            if rgn == 0 {
                return false;
            }
            // SAFETY: HWND from the live Slint window, HRGN just created.
            let applied = unsafe { SetWindowRgn(hwnd, rgn, 1) } != 0;
            if !applied {
                // SAFETY: the region is still ours when SetWindowRgn fails.
                unsafe { DeleteObject(rgn) };
            }
            applied
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn app_window_rounds_and_the_overlay_never_does() {
        assert_eq!(corner_preference(WindowRole::App), DWMWCP_ROUND);
        assert_eq!(corner_preference(WindowRole::Overlay), DWMWCP_DONOTROUND);
        // The two must never agree: one recipe for both windows is the bug.
        assert_ne!(
            corner_preference(WindowRole::App),
            corner_preference(WindowRole::Overlay)
        );
    }

    #[test]
    fn the_maximize_button_restores_a_maximized_window() {
        let normal = max_control(false);
        assert_eq!(normal.glyph, "maximize");
        assert_eq!(normal.label, "Maximize");
        let maximized = max_control(true);
        assert_eq!(maximized.glyph, "restore");
        assert_eq!(maximized.label, "Restore");
        // One button, two states: the glyph and the accessible label must
        // change together, or a screen reader announces the wrong action.
        assert_ne!(normal, maximized);
    }

    #[test]
    fn drag_region_ends_where_the_buttons_begin() {
        let width = 1180.0;
        let layout = titlebar_layout(width);

        // The drag surface is the bar minus the three controls, full height.
        assert_eq!(layout.drag.x, 0.0);
        assert_eq!(layout.drag.width, width - CONTROL_WIDTH * 3.0);
        assert_eq!(layout.drag.height, TITLEBAR_HEIGHT);

        // Controls are contiguous, flush right, and exactly one bar tall.
        assert_eq!(layout.controls[MINIMIZE].x, width - CONTROL_WIDTH * 3.0);
        assert_eq!(
            layout.controls[MAXIMIZE].x,
            layout.controls[MINIMIZE].x + CONTROL_WIDTH
        );
        assert_eq!(
            layout.controls[CLOSE].x,
            layout.controls[MAXIMIZE].x + CONTROL_WIDTH
        );
        assert_eq!(
            layout.controls[CLOSE].x + layout.controls[CLOSE].width,
            width
        );
        let mut expected_x = width - CONTROL_WIDTH * 3.0;
        for control in layout.controls.iter() {
            assert_eq!(control.x, expected_x);
            assert_eq!(control.width, CONTROL_WIDTH);
            assert_eq!(control.y, 0.0);
            assert_eq!(control.height, TITLEBAR_HEIGHT);
            expected_x += CONTROL_WIDTH;
        }
        assert_eq!(expected_x, width);
    }

    #[test]
    fn a_press_in_a_button_is_never_a_drag() {
        let width = 1180.0;
        let layout = titlebar_layout(width);
        // Centre of each button.
        for i in 0..CONTROL_COUNT {
            let rect = layout.controls[i];
            let hit = titlebar_hit(&layout, rect.x + rect.width / 2.0, rect.height / 2.0);
            assert_eq!(hit, TitlebarHit::Control(i), "control {i}");
        }
        // Just left of the first button is still drag surface.
        assert_eq!(
            titlebar_hit(&layout, layout.drag.width - 1.0, TITLEBAR_HEIGHT / 2.0),
            TitlebarHit::Drag
        );
        // The bar's last pixel belongs to close, not to the window edge.
        assert_eq!(
            titlebar_hit(&layout, width - 0.5, 1.0),
            TitlebarHit::Control(CLOSE)
        );
        // Below the bar is neither.
        assert_eq!(
            titlebar_hit(&layout, 10.0, TITLEBAR_HEIGHT + 1.0),
            TitlebarHit::Outside
        );
    }

    #[test]
    fn a_window_narrower_than_its_buttons_has_no_drag_surface() {
        let width = CONTROL_WIDTH * 2.0;
        let layout = titlebar_layout(width);
        assert_eq!(layout.drag.width, 0.0);
        // The controls still occupy their real place, hanging off the left.
        assert_eq!(layout.controls[CLOSE].x + CONTROL_WIDTH, width);
    }

    #[test]
    fn corner_radius_follows_the_monitor_scale_and_the_ellipse_is_a_diameter() {
        assert_eq!(region_radius(1.0), 16); // Shape.lg at 100 %
        assert_eq!(region_radius(1.25), 20);
        assert_eq!(region_radius(1.5), 24);
        assert_eq!(region_radius(2.0), 32);
        // CreateRoundRectRgn wants an ellipse, not a radius.
        assert_eq!(region_ellipse(16), 32);
        assert_eq!(region_ellipse(0), 0);
    }

    #[test]
    fn a_region_is_only_asked_for_when_dwm_did_not_round_and_the_window_is_normal() {
        assert_eq!(corner_mode(0), CornerMode::Dwm);
        // S_OK and any non-negative HRESULT mean DWM took the attribute.
        assert_eq!(corner_mode(1), CornerMode::Dwm);
        assert_eq!(corner_mode(-2147024809), CornerMode::Region);

        // DWM rounds it: never clip, even at a normal size.
        assert_eq!(wanted_region(CornerMode::Dwm, false, 1180, 780, 1.0), None);
        // Maximised: the window owns the work area, so no clip either.
        assert_eq!(
            wanted_region(CornerMode::Region, true, 2560, 1440, 1.0),
            None
        );
        // Fallback at a normal size: the clip follows the window and the DPI.
        assert_eq!(
            wanted_region(CornerMode::Region, false, 1180, 780, 1.0),
            Some(Region {
                width: 1180,
                height: 780,
                radius: 16
            })
        );
        assert_eq!(
            wanted_region(CornerMode::Region, false, 1180, 780, 1.5).map(|r| r.radius),
            Some(24)
        );
        // A zero-sized window is the pre-layout state, not something to clip.
        assert_eq!(wanted_region(CornerMode::Region, false, 0, 0, 1.0), None);
    }

    #[test]
    fn a_region_is_only_reapplied_when_the_needed_clip_changes() {
        let normal = Some(Region {
            width: 1180,
            height: 780,
            radius: 16,
        });
        // Same size on the same monitor: nothing to do.
        assert!(!should_apply_region(normal, normal, None));
        // Restore from maximised: the clip comes back.
        assert!(should_apply_region(None, normal, None));
        // A resize: the clip follows.
        let resized = Some(Region {
            width: 1240,
            height: 800,
            radius: 16,
        });
        assert!(should_apply_region(normal, resized, None));
        // A clip Windows refused at this size is not retried every tick...
        assert!(!should_apply_region(None, normal, normal));
        // ...but a different size gets one fresh attempt.
        assert!(should_apply_region(None, resized, normal));
    }

    #[test]
    fn style_report_requires_every_attribute_to_be_accepted() {
        let ok = StyleReport {
            corner: 0,
            border: 0,
            dark_mode: 0,
        };
        assert!(ok.all_ok());
        for broken in [
            StyleReport { corner: -1, ..ok },
            StyleReport {
                border: -2147024809,
                ..ok
            },
            StyleReport {
                dark_mode: -1,
                ..ok
            },
        ] {
            assert!(!broken.all_ok(), "{broken:?} must not pass");
        }
    }

    #[test]
    fn startup_log_lines_name_the_path_that_was_actually_taken() {
        let report = StyleReport {
            corner: 0,
            border: -1,
            dark_mode: 0,
        };
        let line = report.log_line();
        assert!(line.contains("frameless"));
        assert!(line.contains("corner=ROUND"));
        assert!(line.contains("border=NONE"));
        assert!(line.contains("dark-mode"));
        // A refused attribute shows its HRESULT, not a silent success.
        assert!(line.contains("hr=0xFFFFFFFF"));
        // No caption colour: a frameless window has no caption to paint.
        assert!(!line.contains("caption"));

        // Rounding proof: DWM path names no region, the fallback names the clip.
        assert!(corner_log(CornerMode::Dwm, None, 1.0).contains("DWM ROUND"));
        let region = wanted_region(CornerMode::Region, false, 1180, 780, 1.25);
        let region_line = corner_log(CornerMode::Region, region, 1.25);
        assert!(region_line.contains("1180x780"));
        assert!(region_line.contains("r=20"));
        assert!(region_line.contains("ellipse 40"));
    }
}
