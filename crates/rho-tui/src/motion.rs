//! Reusable oxide-ember motion helpers — rho's TUI "delight" layer.
//!
//! This module is an **owner-sanctioned look/feel divergence** from tau: tau's
//! TUI has no animated working-state signature. rho gets one — a throbbing
//! ember-ρ, shimmering forge-verbs, a breathing composer cursor, and an animated
//! π→τ→ρ heritage splash — all driven from these pure, deterministic helpers so
//! every animated frame is a function of an integer frame index (the 150 ms
//! activity tick) and therefore snapshot-testable at a fixed frame. See
//! `dev-notes/phase-5.md` for the divergence ledger.
//!
//! Two motion primitives, shared by the ρ glyph, the forge-verb, the cursor, and
//! the splash lineage:
//!
//! - **throb** — a sine brightness pulse along the rust-oxide ramp (heated iron
//!   cooling and reheating), NOT a rotating spinner.
//! - **shimmer** — a travelling cosine light-sweep band (a port of Codex's
//!   `shimmer.rs` technique) blended along the OXIDE ramp rather than grey→white.
//!
//! Both degrade to a static, plain rendering when the terminal lacks truecolor or
//! the user asks for reduced motion (Codex is careful here; so are we).

use ratatui::style::{Color, Modifier, Style};
use ratatui::text::Span;

/// The forge/blacksmith working verbs, rotated one per turn (task #45 spec).
pub const WORKING_VERBS: [&str; 10] = [
    "Forging",
    "Oxidizing",
    "Tempering",
    "Annealing",
    "Smelting",
    "Quenching",
    "Kindling",
    "Transmuting",
    "Cogitating",
    "Reticulating",
];

/// Frames per throb cycle at the 150 ms activity tick (~1.5 s → heated-iron beat).
pub const THROB_PERIOD_FRAMES: usize = 10;
/// Frames per shimmer sweep at the 150 ms activity tick (~2.1 s, Codex-like).
pub const SHIMMER_PERIOD_FRAMES: usize = 14;
/// Half-width (in cells) of the shimmer light band (Codex uses ~5).
const SHIMMER_HALF_WIDTH: f32 = 5.0;

/// The rotated working verb for a turn (wraps by turn index).
#[must_use]
pub fn working_verb(turn_index: usize) -> &'static str {
    WORKING_VERBS[turn_index % WORKING_VERBS.len()]
}

/// Terminal motion capabilities: whether we may animate (truecolor) and whether
/// the user opted out of motion. Resolved once from the environment and threaded
/// through the render context; tests construct it explicitly to exercise both the
/// animated and the plain-fallback paths.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MotionCaps {
    /// The terminal advertises 24-bit color (needed for the oxide ramp).
    pub truecolor: bool,
    /// The user asked for reduced motion (`RHO_REDUCED_MOTION`, `NO_COLOR`,
    /// `TERM=dumb`), so animations hold on a static frame.
    pub reduced_motion: bool,
}

impl MotionCaps {
    /// Resolve motion capabilities from the environment.
    ///
    /// - truecolor: `COLORTERM` is `truecolor`/`24bit` and color is not disabled.
    /// - reduced motion: `RHO_REDUCED_MOTION` truthy, `NO_COLOR` set, or a dumb
    ///   terminal.
    #[must_use]
    pub fn from_env() -> Self {
        let no_color = std::env::var_os("NO_COLOR").is_some();
        let dumb = std::env::var("TERM").is_ok_and(|t| t == "dumb");
        let colorterm = std::env::var("COLORTERM").unwrap_or_default();
        let truecolor = !no_color && !dumb && matches!(colorterm.as_str(), "truecolor" | "24bit");
        let reduced_motion =
            no_color || dumb || std::env::var("RHO_REDUCED_MOTION").is_ok_and(|v| is_truthy(&v));
        Self {
            truecolor,
            reduced_motion,
        }
    }

    /// Fully-static capabilities (the safe fallback used in reduced-motion /
    /// non-truecolor contexts and as a test default).
    #[must_use]
    pub const fn plain() -> Self {
        Self {
            truecolor: false,
            reduced_motion: true,
        }
    }

    /// Fully-animated capabilities (truecolor, motion on) — the common desktop
    /// terminal, and the explicit choice in animated snapshot tests.
    #[must_use]
    pub const fn animated_caps() -> Self {
        Self {
            truecolor: true,
            reduced_motion: false,
        }
    }

    /// Whether animated motion should play (truecolor available and not reduced).
    #[must_use]
    pub const fn animated(self) -> bool {
        self.truecolor && !self.reduced_motion
    }
}

fn is_truthy(value: &str) -> bool {
    matches!(
        value.trim().to_ascii_lowercase().as_str(),
        "1" | "true" | "yes" | "on"
    )
}

// --- the oxide ramp ---------------------------------------------------------

/// Control stops of the rust-oxide ramp: dim terracotta → rho oxide (#b3391f) →
/// hot gold. `oxide_ramp` interpolates between them.
const RAMP: [(u8, u8, u8); 3] = [
    (0x5a, 0x1e, 0x10), // dim terracotta (cool iron)
    (0xb3, 0x39, 0x1f), // rho oxide (#b3391f)
    (0xe6, 0xa5, 0x4a), // hot gold (reheated iron)
];

/// A color along the rust-oxide ramp for `t` in `[0, 1]` (clamped): `0` dim
/// terracotta, `~0.5` rho oxide, `1` hot gold.
#[must_use]
pub fn oxide_ramp(t: f32) -> Color {
    let t = t.clamp(0.0, 1.0);
    let scaled = t * 2.0; // two segments across three stops
    let (lo, hi, local) = if scaled <= 1.0 {
        (RAMP[0], RAMP[1], scaled)
    } else {
        (RAMP[1], RAMP[2], scaled - 1.0)
    };
    Color::Rgb(
        lerp_u8(lo.0, hi.0, local),
        lerp_u8(lo.1, hi.1, local),
        lerp_u8(lo.2, hi.2, local),
    )
}

fn lerp_u8(a: u8, b: u8, t: f32) -> u8 {
    let a = f32::from(a);
    let b = f32::from(b);
    #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
    let v = (a + (b - a) * t).round().clamp(0.0, 255.0) as u8;
    v
}

// --- throb (brightness pulse) -----------------------------------------------

/// The throb intensity for `frame` on a `period`-frame sine, in `[0, 1]`.
#[must_use]
pub fn throb01(frame: usize, period: usize) -> f32 {
    if period == 0 {
        return 1.0;
    }
    #[allow(clippy::cast_precision_loss)]
    let phase = (frame % period) as f32 / period as f32;
    0.5 + 0.5 * (std::f32::consts::TAU * phase).sin()
}

/// The throbbing ember style for the prompt-prefix ρ.
///
/// Animated: the ρ breathes along the oxide ramp (dim terracotta ↔ hot gold),
/// bold. Static (idle, or reduced-motion / non-truecolor): a quiet dim-oxide ρ.
#[must_use]
pub fn ember_throb_style(caps: MotionCaps, frame: usize, running: bool) -> Style {
    if !running {
        // Idle: a settled, dim ρ — present but at rest.
        return Style::default()
            .fg(if caps.truecolor {
                oxide_ramp(0.28)
            } else {
                Color::Red
            })
            .add_modifier(Modifier::DIM);
    }
    if !caps.animated() {
        // Running but no motion budget: a steady bold oxide ρ.
        return Style::default()
            .fg(if caps.truecolor {
                oxide_ramp(0.5)
            } else {
                Color::Red
            })
            .add_modifier(Modifier::BOLD);
    }
    // Running, animated: throb the brightness so the ρ reads as heated iron.
    let t = 0.2 + 0.8 * throb01(frame, THROB_PERIOD_FRAMES);
    Style::default()
        .fg(oxide_ramp(t))
        .add_modifier(Modifier::BOLD)
}

/// A soft oxide cursor style for the composer, breathing while idle.
///
/// Animated: a reversed cell whose oxide tint pulses gently (same ember family,
/// quieter than the ρ). Static: a plain reversed block (the terminal default).
#[must_use]
pub fn cursor_throb_style(caps: MotionCaps, frame: usize) -> Style {
    if !caps.animated() {
        return Style::default().add_modifier(Modifier::REVERSED);
    }
    // A gentle pulse over a wider band so the block cursor stays legible.
    let t = 0.45 + 0.4 * throb01(frame, THROB_PERIOD_FRAMES);
    Style::default()
        .bg(oxide_ramp(t))
        .fg(Color::Rgb(0x14, 0x10, 0x0e))
}

// --- shimmer (travelling light sweep) ---------------------------------------

/// Render `text` as a shimmering forge-verb: a cosine light-sweep band travels
/// along the string, brightening a few cells at a time up the oxide ramp (a port
/// of Codex's `shimmer.rs`, blended over oxide rather than grey→white).
///
/// Falls back to a single plain-oxide span when motion is unavailable.
#[must_use]
pub fn shimmer_spans(text: &str, caps: MotionCaps, frame: usize) -> Vec<Span<'static>> {
    if !caps.animated() {
        let color = if caps.truecolor {
            oxide_ramp(0.45)
        } else {
            Color::Red
        };
        return vec![Span::styled(text.to_string(), Style::default().fg(color))];
    }
    let chars: Vec<char> = text.chars().collect();
    let n = chars.len();
    if n == 0 {
        return Vec::new();
    }
    #[allow(clippy::cast_precision_loss)]
    let span_range = n as f32 + 2.0 * SHIMMER_HALF_WIDTH;
    #[allow(clippy::cast_precision_loss)]
    let phase = (frame % SHIMMER_PERIOD_FRAMES) as f32 / SHIMMER_PERIOD_FRAMES as f32;
    let center = phase * span_range - SHIMMER_HALF_WIDTH;
    let mut spans = Vec::with_capacity(n);
    for (i, ch) in chars.into_iter().enumerate() {
        #[allow(clippy::cast_precision_loss)]
        let dist = (i as f32 - center).abs();
        let m = if dist < SHIMMER_HALF_WIDTH {
            0.5 + 0.5 * (std::f32::consts::PI * dist / SHIMMER_HALF_WIDTH).cos()
        } else {
            0.0
        };
        // Base oxide, brightened up the ramp where the band passes.
        let t = 0.35 + 0.65 * m;
        spans.push(Span::styled(
            ch.to_string(),
            Style::default().fg(oxide_ramp(t)),
        ));
    }
    spans
}

// --- elapsed timer ----------------------------------------------------------

/// Format elapsed working time in Codex's compact form: `0s`, `1m 00s`,
/// `2h 03m 09s` (zero-padded once past the leading unit).
#[must_use]
pub fn format_working_elapsed(seconds: u64) -> String {
    if seconds < 60 {
        return format!("{seconds}s");
    }
    if seconds < 3600 {
        let (m, s) = (seconds / 60, seconds % 60);
        return format!("{m}m {s:02}s");
    }
    let (h, rem) = (seconds / 3600, seconds % 3600);
    let (m, s) = (rem / 60, rem % 60);
    format!("{h}h {m:02}m {s:02}s")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn oxide_ramp_endpoints_and_midpoint() {
        assert_eq!(oxide_ramp(0.0), Color::Rgb(0x5a, 0x1e, 0x10));
        assert_eq!(oxide_ramp(0.5), Color::Rgb(0xb3, 0x39, 0x1f));
        assert_eq!(oxide_ramp(1.0), Color::Rgb(0xe6, 0xa5, 0x4a));
        // Clamps out-of-range inputs.
        assert_eq!(oxide_ramp(-1.0), oxide_ramp(0.0));
        assert_eq!(oxide_ramp(2.0), oxide_ramp(1.0));
    }

    #[test]
    fn throb_is_periodic_and_bounded() {
        for period in [1usize, 10, 14] {
            for frame in 0..(period * 3) {
                let v = throb01(frame, period);
                assert!((0.0..=1.0).contains(&v), "v={v}");
                // Same phase → same value across cycles.
                assert!((throb01(frame, period) - throb01(frame + period, period)).abs() < 1e-5);
            }
        }
    }

    #[test]
    fn ember_static_when_idle_or_no_motion() {
        // Idle: dim, never bold.
        let idle = ember_throb_style(MotionCaps::animated_caps(), 3, false);
        assert!(idle.add_modifier.contains(Modifier::DIM));
        assert!(!idle.add_modifier.contains(Modifier::BOLD));
        // Running but no truecolor: steady bold, and identical across frames.
        let a = ember_throb_style(MotionCaps::plain(), 0, true);
        let b = ember_throb_style(MotionCaps::plain(), 7, true);
        assert_eq!(a, b, "no-motion running ρ must not animate");
        assert!(a.add_modifier.contains(Modifier::BOLD));
    }

    #[test]
    fn ember_throbs_when_running_animated() {
        let caps = MotionCaps::animated_caps();
        // Two different phases of the cycle differ in color.
        let peak = ember_throb_style(caps, THROB_PERIOD_FRAMES / 4, true); // sin peak
        let trough = ember_throb_style(caps, 3 * THROB_PERIOD_FRAMES / 4, true); // sin trough
        assert_ne!(peak.fg, trough.fg, "throb must vary brightness");
    }

    #[test]
    fn shimmer_plain_fallback_is_single_span() {
        let spans = shimmer_spans("Tempering", MotionCaps::plain(), 0);
        assert_eq!(spans.len(), 1);
        assert_eq!(spans[0].content, "Tempering");
    }

    #[test]
    fn shimmer_animated_is_per_char_and_moves() {
        let caps = MotionCaps::animated_caps();
        let a = shimmer_spans("Tempering", caps, 0);
        // One span per character.
        assert_eq!(a.len(), "Tempering".chars().count());
        // The band position advances with the frame, so the styling differs.
        let b = shimmer_spans("Tempering", caps, SHIMMER_PERIOD_FRAMES / 2);
        let colors_a: Vec<_> = a.iter().map(|s| s.style.fg).collect();
        let colors_b: Vec<_> = b.iter().map(|s| s.style.fg).collect();
        assert_ne!(colors_a, colors_b, "shimmer band must travel");
    }

    #[test]
    fn working_verbs_rotate() {
        assert_eq!(working_verb(0), "Forging");
        assert_eq!(working_verb(2), "Tempering");
        assert_eq!(working_verb(WORKING_VERBS.len()), "Forging");
    }

    #[test]
    fn elapsed_uses_codex_format() {
        assert_eq!(format_working_elapsed(0), "0s");
        assert_eq!(format_working_elapsed(9), "9s");
        assert_eq!(format_working_elapsed(60), "1m 00s");
        assert_eq!(format_working_elapsed(134), "2m 14s");
        assert_eq!(format_working_elapsed(7389), "2h 03m 09s");
    }

    #[test]
    fn caps_animated_requires_truecolor_and_motion() {
        assert!(MotionCaps::animated_caps().animated());
        assert!(!MotionCaps::plain().animated());
        assert!(
            !MotionCaps {
                truecolor: true,
                reduced_motion: true
            }
            .animated()
        );
    }
}
