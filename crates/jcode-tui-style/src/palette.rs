//! Semantic palette with 12 roles mapped to three color tiers.
//!
//! Roles are the single source of truth for jcode's colors (§2.1 of the
//! TUI redesign spec).  Each role maps to a specific truecolor hex, a
//! fixed 256-color palette index, and a named ANSI-16 slot so every tier
//! is a designed experience, not a broken rich screen.

use ratatui::style::Color;

use crate::color::{ColorCapability, color_capability};

// ── Role ──────────────────────────────────────────────────────────────

/// Twelve semantic roles for the jcode TUI palette.
///
/// The user-identity role is named `SelfRole` to avoid the Rust `Self`
/// keyword while keeping the semantic intent clear.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Role {
    /// Primary message text and values.  `#F5F5FF`
    TextPrimary,
    /// Assistant body text.  `#DCDCD7`
    TextSecondary,
    /// Metadata, timestamps, line numbers.  `#787878`
    Muted,
    /// Decoration only (never for information).  `#505050`
    Faint,
    /// User-message surface background.  `#232832`
    Surface1,
    /// Overlay and picker-row background.  `#171B23`
    Surface2,
    /// Focus, selection, active-pane edge.  `#BA8BFF`
    Accent,
    /// User identity and gutter bar.  `#8AB4F8`
    SelfRole,
    /// Assistant identity and success states.  `#81C784`
    Agent,
    /// Queued items, rate-limit, cache warnings.  `#FFC107`
    Warn,
    /// Failures and destructive confirmations.  `#FF8A80`
    Error,
    /// Links, hints, and transport notes.  `#6ED2FF`
    Info,
}

// ── Tier ──────────────────────────────────────────────────────────────

/// Color-precision tier.
///
/// `Rich` and `Ansi256` are detected from the terminal environment via
/// the existing [`color_capability`] logic.  `Plain` (ANSI-16) is *not*
/// detected automatically; it is forced by `NO_COLOR`, a dumb terminal,
/// or a user override (to be implemented in WP 2).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Tier {
    /// Truecolor (24‑bit RGB).
    Rich,
    /// 256‑color indexed palette.
    Ansi256,
    /// ANSI‑16 named slots.
    Plain,
}

// ── Tier detection ────────────────────────────────────────────────────

/// Map the existing [`ColorCapability`] to a [`Tier`].
///
/// `ColorCapability::TrueColor`  → `Tier::Rich`
/// `ColorCapability::Color256`   → `Tier::Ansi256`
///
/// `Tier::Plain` is never returned here; it must be forced externally.
pub fn detect_tier() -> Tier {
    match color_capability() {
        ColorCapability::TrueColor => Tier::Rich,
        ColorCapability::Color256  => Tier::Ansi256,
    }
}

// ── role_color ────────────────────────────────────────────────────────

/// Return the [`Color`] for a semantic [`Role`] at a given [`Tier`].
pub fn role_color(role: Role, tier: Tier) -> Color {
    match tier {
        Tier::Rich    => role_truecolor(role),
        Tier::Ansi256 => role_ansi256(role),
        Tier::Plain   => role_plain(role),
    }
}

// ── Per-tier helpers ──────────────────────────────────────────────────

fn role_truecolor(role: Role) -> Color {
    match role {
        Role::TextPrimary   => Color::Rgb(245, 245, 255), // #F5F5FF
        Role::TextSecondary => Color::Rgb(220, 220, 215), // #DCDCD7
        Role::Muted         => Color::Rgb(120, 120, 120), // #787878
        Role::Faint         => Color::Rgb( 80,  80,  80), // #505050
        Role::Surface1      => Color::Rgb( 35,  40,  50), // #232832
        Role::Surface2      => Color::Rgb( 23,  27,  35), // #171B23
        Role::Accent        => Color::Rgb(186, 139, 255), // #BA8BFF
        Role::SelfRole      => Color::Rgb(138, 180, 248), // #8AB4F8
        Role::Agent         => Color::Rgb(129, 199, 132), // #81C784
        Role::Warn          => Color::Rgb(255, 193,   7), // #FFC107
        Role::Error         => Color::Rgb(255, 138, 128), // #FF8A80
        Role::Info          => Color::Rgb(110, 210, 255), // #6ED2FF
    }
}

fn role_ansi256(role: Role) -> Color {
    match role {
        Role::TextPrimary   => Color::Indexed(255),
        Role::TextSecondary => Color::Indexed(253),
        Role::Muted         => Color::Indexed(243),
        Role::Faint         => Color::Indexed(239),
        Role::Surface1      => Color::Indexed(235),
        Role::Surface2      => Color::Indexed(234),
        Role::Accent        => Color::Indexed(141),
        Role::SelfRole      => Color::Indexed(111),
        Role::Agent         => Color::Indexed(114),
        Role::Warn          => Color::Indexed(214),
        Role::Error         => Color::Indexed(210),
        Role::Info          => Color::Indexed( 81),
    }
}

fn role_plain(role: Role) -> Color {
    match role {
        Role::TextPrimary   => Color::White,        // Bright White (15)
        Role::TextSecondary => Color::Gray,          // White (7)
        Role::Muted         => Color::DarkGray,      // Bright Black (8)
        Role::Faint         => Color::DarkGray,      // Bright Black + dim (caller adds DIM)
        Role::Surface1      => Color::Reset,         // default bg
        Role::Surface2      => Color::Reset,         // default bg
        Role::Accent        => Color::LightMagenta,  // Bright Magenta (13)
        Role::SelfRole      => Color::LightBlue,     // Bright Blue (12)
        Role::Agent         => Color::LightGreen,    // Bright Green (10)
        Role::Warn          => Color::LightYellow,   // Bright Yellow (11)
        Role::Error         => Color::LightRed,      // Bright Red (9)
        Role::Info          => Color::LightCyan,     // Bright Cyan (14)
    }
}

// ── debug_palette_json ────────────────────────────────────────────────

/// Return a JSON string enumerating every `Role × Tier` mapping.
///
/// Hand-rolled to avoid adding a serde dependency.
pub fn debug_palette_json() -> String {
    let roles: &[(Role, &str)] = &[
        (Role::TextPrimary,   "text-primary"),
        (Role::TextSecondary, "text-secondary"),
        (Role::Muted,         "muted"),
        (Role::Faint,         "faint"),
        (Role::Surface1,      "surface-1"),
        (Role::Surface2,      "surface-2"),
        (Role::Accent,        "accent"),
        (Role::SelfRole,      "self"),
        (Role::Agent,         "agent"),
        (Role::Warn,          "warn"),
        (Role::Error,         "error"),
        (Role::Info,          "info"),
    ];

    let tiers: &[(Tier, &str)] = &[
        (Tier::Rich,    "Rich"),
        (Tier::Ansi256, "Ansi256"),
        (Tier::Plain,   "Plain"),
    ];

    let mut json = String::from("{\"roles\":[");
    for (i, (role, role_name)) in roles.iter().enumerate() {
        if i > 0 {
            json.push(',');
        }
        json.push('{');
        json.push_str(&format!("\"role\":\"{role_name}\",\"tiers\":{{"));
        for (j, (tier, tier_name)) in tiers.iter().enumerate() {
            if j > 0 {
                json.push(',');
            }
            let color = role_color(*role, *tier);
            json.push('"');
            json.push_str(tier_name);
            json.push('"');
            json.push(':');
            json.push_str(&color_to_json(&color));
        }
        json.push('}');
        json.push('}');
    }
    json.push(']');
    json.push('}');
    json
}

fn color_to_json(c: &Color) -> String {
    match c {
        Color::Rgb(r, g, b) => {
            format!("{{\"Rgb\":[{r},{g},{b}]}}")
        }
        Color::Indexed(n) => {
            format!("{{\"Indexed\":{n}}}")
        }
        Color::Reset => "\"Reset\"".to_string(),
        other => format!("\"{other:?}\""),
    }
}

// ── Tests ─────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashSet;

    // ── Rich / truecolor spot-checks ────────────────────────────────

    #[test]
    fn text_primary_rich_is_f5f5ff() {
        assert_eq!(
            role_color(Role::TextPrimary, Tier::Rich),
            Color::Rgb(245, 245, 255),
        );
    }

    #[test]
    fn self_role_rich_is_8ab4f8() {
        assert_eq!(
            role_color(Role::SelfRole, Tier::Rich),
            Color::Rgb(138, 180, 248),
        );
    }

    #[test]
    fn accent_rich_is_ba8bff() {
        assert_eq!(
            role_color(Role::Accent, Tier::Rich),
            Color::Rgb(186, 139, 255),
        );
    }

    #[test]
    fn surface1_rich_is_232832() {
        assert_eq!(
            role_color(Role::Surface1, Tier::Rich),
            Color::Rgb(35, 40, 50),
        );
    }

    // ── Ansi256 / 256-color spot-checks ─────────────────────────────

    #[test]
    fn error_ansi256_is_210() {
        assert_eq!(
            role_color(Role::Error, Tier::Ansi256),
            Color::Indexed(210),
        );
    }

    #[test]
    fn warn_ansi256_is_214() {
        assert_eq!(
            role_color(Role::Warn, Tier::Ansi256),
            Color::Indexed(214),
        );
    }

    #[test]
    fn agent_ansi256_is_114() {
        assert_eq!(
            role_color(Role::Agent, Tier::Ansi256),
            Color::Indexed(114),
        );
    }

    #[test]
    fn info_ansi256_is_81() {
        assert_eq!(
            role_color(Role::Info, Tier::Ansi256),
            Color::Indexed(81),
        );
    }

    // ── Plain / ANSI-16 spot-checks ─────────────────────────────────

    #[test]
    fn self_role_plain_is_light_blue() {
        assert_eq!(
            role_color(Role::SelfRole, Tier::Plain),
            Color::LightBlue,
        );
    }

    #[test]
    fn agent_plain_is_light_green() {
        assert_eq!(
            role_color(Role::Agent, Tier::Plain),
            Color::LightGreen,
        );
    }

    #[test]
    fn error_plain_is_light_red() {
        assert_eq!(
            role_color(Role::Error, Tier::Plain),
            Color::LightRed,
        );
    }

    #[test]
    fn info_plain_is_light_cyan() {
        assert_eq!(
            role_color(Role::Info, Tier::Plain),
            Color::LightCyan,
        );
    }

    #[test]
    fn surface_plain_is_reset() {
        assert_eq!(role_color(Role::Surface1, Tier::Plain), Color::Reset);
        assert_eq!(role_color(Role::Surface2, Tier::Plain), Color::Reset);
    }

    #[test]
    fn muted_plain_is_dark_gray() {
        assert_eq!(role_color(Role::Muted, Tier::Plain), Color::DarkGray);
    }

    #[test]
    fn faint_plain_is_dark_gray() {
        assert_eq!(role_color(Role::Faint, Tier::Plain), Color::DarkGray);
    }

    // ── Cross-tier invariants ───────────────────────────────────────

    #[test]
    fn all_rich_colors_are_distinct() {
        let all = all_roles();
        let mut seen = HashSet::new();
        for role in all {
            let c = role_color(role, Tier::Rich);
            let key = match c {
                Color::Rgb(r, g, b) => (r, g, b),
                other => panic!("Rich tier should produce Rgb, got {other:?}"),
            };
            assert!(seen.insert(key), "duplicate truecolor for {role:?}");
        }
        assert_eq!(seen.len(), 12);
    }

    #[test]
    fn all_ansi256_indices_are_distinct() {
        let all = all_roles();
        let mut seen = HashSet::new();
        for role in all {
            let c = role_color(role, Tier::Ansi256);
            let idx = match c {
                Color::Indexed(n) => n,
                other => panic!("Ansi256 tier should produce Indexed, got {other:?}"),
            };
            assert!(seen.insert(idx), "duplicate 256 index for {role:?}");
        }
        assert_eq!(seen.len(), 12);
    }

    // ── debug_palette_json ──────────────────────────────────────────

    #[test]
    fn debug_palette_json_contains_all_roles_and_tiers() {
        let json = debug_palette_json();
        for name in &[
            "text-primary", "text-secondary", "muted", "faint",
            "surface-1", "surface-2", "accent", "self", "agent",
            "warn", "error", "info",
        ] {
            assert!(
                json.contains(name),
                "debug json missing role: {name}"
            );
        }
        for tier in &["Rich", "Ansi256", "Plain"] {
            assert!(
                json.contains(tier),
                "debug json missing tier: {tier}"
            );
        }
        let trimmed = json.trim();
        assert!(trimmed.starts_with('{'), "not a JSON object");
        assert!(trimmed.ends_with('}'), "not a JSON object");
    }

    // ── Helpers ─────────────────────────────────────────────────────

    fn all_roles() -> [Role; 12] {
        [
            Role::TextPrimary,
            Role::TextSecondary,
            Role::Muted,
            Role::Faint,
            Role::Surface1,
            Role::Surface2,
            Role::Accent,
            Role::SelfRole,
            Role::Agent,
            Role::Warn,
            Role::Error,
            Role::Info,
        ]
    }
}
