//! A link as a QR code that a phone scans off the terminal.

use qrcode::{QrCode, render::unicode::Dense1x2};

/// Black on white from the fixed part of the 256-colour palette. A theme
/// restyles the default pair and the first 16 colours, so either could draw
/// the code inverted or too faint to scan.
const BLACK_ON_WHITE: &str = "\x1b[38;5;16;48;5;231m";
const RESET: &str = "\x1b[0m";

pub(crate) struct TerminalCode {
    /// Terminal cells each row fills. A narrower terminal wraps the rows,
    /// and a wrapped code cannot be scanned.
    pub columns: usize,
    pub rows: Vec<String>,
}

/// Two modules a cell, one above the other, with the quiet zone drawn so the
/// code never borders the terminal's own background. `None` when the link is
/// too long for a QR code.
pub(crate) fn terminal_code(link: &str) -> Option<TerminalCode> {
    let drawn = QrCode::new(link).ok()?.render::<Dense1x2>().build();
    let columns = drawn.lines().next()?.chars().count();
    let rows = drawn
        .lines()
        .map(|cells| format!("{BLACK_ON_WHITE}{cells}{RESET}"))
        .collect();
    Some(TerminalCode { columns, rows })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::scan::{Theme, scan};

    const LINK: &str =
        "https://device.tail123.ts.net/w/0123456789abcdef0123?n=0123456789abcdef0123456789abcdef";

    fn long_host() -> String {
        format!(
            "https://{}.tail-scale-name.ts.net:8443/w/0123456789abcdef0123?n=0123456789abcdef0123456789abcdef",
            "a".repeat(63)
        )
    }

    fn drawn(link: &str) -> String {
        terminal_code(link).unwrap().rows.join("\n")
    }

    #[test]
    fn a_scanner_reads_the_exact_link_on_dark_and_light_themes() {
        let long_host = long_host();
        for link in [LINK, "https://a.ts.net/w/0?n=1", &long_host] {
            for theme in [Theme::Dark, Theme::Light] {
                assert_eq!(
                    scan(&drawn(link), theme).as_deref(),
                    Some(link),
                    "{theme:?}"
                );
            }
        }
    }

    #[test]
    fn the_scanner_needs_the_drawn_colours_on_a_dark_theme() {
        let unpainted = drawn(LINK)
            .replace(BLACK_ON_WHITE, "\x1b[0m")
            .replace(RESET, "\x1b[0m");
        assert_eq!(scan(&unpainted, Theme::Light).as_deref(), Some(LINK));
        assert_eq!(scan(&unpainted, Theme::Dark), None);
    }

    #[test]
    fn a_tailnet_link_fits_an_ordinary_terminal() {
        // Digits pack tighter than letters, so a link with none is the
        // largest code its length gives.
        let widest = |origin: &str| {
            terminal_code(&format!(
                "{origin}/w/{}?n={}",
                "f".repeat(20),
                "f".repeat(32)
            ))
            .unwrap()
        };
        let code = widest("https://device.tail123.ts.net");
        assert_eq!((code.columns, code.rows.len()), (49, 25));
        for row in &code.rows {
            let cells = row
                .strip_prefix(BLACK_ON_WHITE)
                .and_then(|row| row.strip_suffix(RESET))
                .unwrap();
            assert_eq!(cells.chars().count(), code.columns);
            assert!(cells.chars().all(|cell| " ▀▄█".contains(cell)), "{cells}");
        }
        // A machine name of 63 characters, on a port other than 443.
        let longest = widest(&format!(
            "https://{}.tail-scale-name.ts.net:8443",
            "a".repeat(63)
        ));
        assert_eq!((longest.columns, longest.rows.len()), (61, 31));
    }

    #[test]
    fn a_link_too_long_for_a_code_draws_nothing() {
        assert!(terminal_code(&"a".repeat(8_000)).is_none());
    }
}
