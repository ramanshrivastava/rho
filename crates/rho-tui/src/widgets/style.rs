//! Tau theme-string -> ratatui `Style` parsing.
//!
//! [`crate::theme::TuiTheme`] keeps its colors as tau's exact style strings
//! (`"#d8dee9 on #000000"`, `"bold #061a1a on #a7f3f0"`, `"#667085"`, …) so the
//! palette matches tau byte-for-byte per theme. This module parses those strings
//! into ratatui [`Style`]s at the render layer — the conversion tau defers to
//! Rich/Textual's style parser.
//!
//! The grammar handled here is the subset tau's themes actually emit: optional
//! leading modifier words (`bold`, `italic`, `dim`, `underline`, …), an optional
//! foreground color, the literal `on`, and an optional background color. Colors
//! are `#rrggbb` hex or a ratatui named color (`black`, `white`, …).

use std::str::FromStr;

use ratatui::style::{Color, Modifier, Style};

use crate::theme::{TuiRoleStyle, TuiTheme};

/// Parse a tau style string into a ratatui [`Style`].
///
/// Unknown tokens are ignored (matching Rich's permissive parser) rather than
/// failing, so a future theme field never breaks rendering.
#[must_use]
pub fn parse_style(spec: &str) -> Style {
    let spec = spec.trim();
    if spec.is_empty() {
        return Style::default();
    }
    let mut style = Style::default();
    let mut after_on = false;
    for raw in spec.split_whitespace() {
        let token = raw.trim_end_matches(',');
        match token.to_ascii_lowercase().as_str() {
            "on" => after_on = true,
            "bold" => style = style.add_modifier(Modifier::BOLD),
            "italic" => style = style.add_modifier(Modifier::ITALIC),
            "dim" => style = style.add_modifier(Modifier::DIM),
            "underline" | "underlined" => style = style.add_modifier(Modifier::UNDERLINED),
            "strike" | "crossed" => style = style.add_modifier(Modifier::CROSSED_OUT),
            "reverse" | "reversed" => style = style.add_modifier(Modifier::REVERSED),
            "blink" | "slow_blink" => style = style.add_modifier(Modifier::SLOW_BLINK),
            "rapid_blink" => style = style.add_modifier(Modifier::RAPID_BLINK),
            "none" | "default" => {}
            _ => match parse_color(token) {
                Some(color) => {
                    if after_on {
                        style = style.bg(color);
                    } else {
                        style = style.fg(color);
                    }
                }
                None => {}
            },
        }
    }
    style
}

/// Parse a single tau color token (`"#rrggbb"` or a named color) into a ratatui
/// [`Color`], or `None` if it is not a recognized color.
#[must_use]
pub fn parse_color(token: &str) -> Option<Color> {
    let token = token.trim();
    if token.is_empty() {
        return None;
    }
    Color::from_str(token).ok()
}

/// A role block's resolved border + body ratatui styles.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RoleStyles {
    /// The left-gutter / border style.
    pub border: Style,
    /// The body text style.
    pub body: Style,
}

impl RoleStyles {
    /// Resolve a tau [`TuiRoleStyle`] into ratatui styles.
    #[must_use]
    pub fn from_role(role: &TuiRoleStyle) -> Self {
        Self {
            border: parse_style(&role.border),
            body: parse_style(&role.body),
        }
    }
}

/// Look up and resolve the role styles for a transcript role name.
///
/// Falls back to the `custom` role (tau's default for unknown roles) so an
/// unrecognized role still renders with a sane palette.
#[must_use]
pub fn role_styles(theme: &TuiTheme, role: &str) -> RoleStyles {
    let resolved = theme
        .role_style(role)
        .or_else(|| theme.role_style("custom"));
    match resolved {
        Some(role_style) => RoleStyles::from_role(role_style),
        None => RoleStyles {
            border: parse_style(&theme.border),
            body: parse_style(&theme.screen_text),
        },
    }
}

/// Resolve the styles for a [`crate::state::ChatItemRole`].
#[must_use]
pub fn chat_role_styles(theme: &TuiTheme, role: crate::state::ChatItemRole) -> RoleStyles {
    role_styles(theme, role.as_str())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_fg_only_hex() {
        let style = parse_style("#667085");
        assert_eq!(style.fg, Some(Color::Rgb(0x66, 0x70, 0x85)));
        assert_eq!(style.bg, None);
        assert_eq!(style.add_modifier, Modifier::empty());
    }

    #[test]
    fn parses_fg_on_bg_hex() {
        let style = parse_style("#d8dee9 on #000000");
        assert_eq!(style.fg, Some(Color::Rgb(0xd8, 0xde, 0xe9)));
        assert_eq!(style.bg, Some(Color::Rgb(0x00, 0x00, 0x00)));
    }

    #[test]
    fn parses_bold_fg_on_bg() {
        let style = parse_style("bold #061a1a on #a7f3f0");
        assert_eq!(style.fg, Some(Color::Rgb(0x06, 0x1a, 0x1a)));
        assert_eq!(style.bg, Some(Color::Rgb(0xa7, 0xf3, 0xf0)));
        assert_eq!(style.add_modifier, Modifier::BOLD);
    }

    #[test]
    fn parses_named_colors() {
        let style = parse_style("black on #7fffd4");
        assert_eq!(style.fg, Some(Color::Black));
        assert_eq!(style.bg, Some(Color::Rgb(0x7f, 0xff, 0xd4)));
    }

    #[test]
    fn empty_string_is_default() {
        assert_eq!(parse_style(""), Style::default());
        assert_eq!(parse_style("   "), Style::default());
    }

    #[test]
    fn unknown_tokens_are_ignored() {
        let style = parse_style("bold weirdcolor on #000000");
        assert_eq!(style.add_modifier, Modifier::BOLD);
        assert_eq!(style.bg, Some(Color::Rgb(0, 0, 0)));
        assert_eq!(style.fg, None);
    }

    #[test]
    fn resolves_chat_role_styles() {
        let theme = crate::theme::tau_dark_theme();
        let user = chat_role_styles(&theme, crate::state::ChatItemRole::User);
        assert_eq!(user.border.fg, Some(Color::Rgb(0x7c, 0x8e, 0xa6)));
        assert_eq!(user.body.fg, Some(Color::Rgb(0xd8, 0xde, 0xe9)));
        assert_eq!(user.body.bg, Some(Color::Rgb(0, 0, 0)));
    }
}
