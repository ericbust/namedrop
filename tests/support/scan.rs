#![allow(dead_code)]
//! Reads a QR code off terminal output the way a phone camera does: by the
//! colours a terminal would show, not by the characters printed.

#[derive(Clone, Copy, Debug)]
pub enum Theme {
    Dark,
    Light,
}

/// What a scanner reads from the code drawn in `output` on a terminal with
/// this theme, or `None` when it finds no code it can read. Rows of the code
/// are the lines with block characters or a fixed colour; other lines are text.
pub fn scan(output: &str, theme: Theme) -> Option<String> {
    let dark_background = matches!(theme, Theme::Dark);
    let rows: Vec<Vec<(bool, bool)>> = output
        .lines()
        .filter(|line| {
            line.contains(['▀', '▄', '█'])
                || line.contains("\x1b[38;5;")
                || line.contains("\x1b[48;5;")
        })
        .map(|line| shown(line, dark_background))
        .collect();
    let columns = rows.iter().map(Vec::len).max()?;
    // The terminal's own background surrounds the code on every side.
    const MARGIN: usize = 6;
    const SCALE: usize = 4;
    let width = (columns + 2 * MARGIN) * SCALE;
    let height = (rows.len() + MARGIN) * 2 * SCALE;
    let dark_at = |x: usize, y: usize| {
        let (column, half) = (x / SCALE, y / SCALE);
        let cell = (half / 2)
            .checked_sub(MARGIN / 2)
            .and_then(|row| rows.get(row))
            .and_then(|row| row.get(column.checked_sub(MARGIN)?));
        match cell {
            Some((top, _)) if half.is_multiple_of(2) => *top,
            Some((_, bottom)) => *bottom,
            None => dark_background,
        }
    };
    let mut image = rqrr::PreparedImage::prepare_from_greyscale(width, height, |x, y| {
        if dark_at(x, y) { 0 } else { 255 }
    });
    let grids = image.detect_grids();
    let [grid] = grids.as_slice() else {
        return None;
    };
    grid.decode().ok().map(|(_, content)| content)
}

/// Whether the top and the bottom half of each cell in `line` show dark.
fn shown(line: &str, dark_background: bool) -> Vec<(bool, bool)> {
    let (mut ink_dark, mut paper_dark) = (!dark_background, dark_background);
    let mut cells = Vec::new();
    let mut rest = line;
    while let Some(character) = rest.chars().next() {
        if let Some(sequence) = rest.strip_prefix("\x1b[") {
            let (codes, after) = sequence.split_once('m').expect("an unfinished colour");
            let codes: Vec<&str> = codes.split(';').collect();
            let mut codes = codes.as_slice();
            while let Some((code, following)) = codes.split_first() {
                codes = match (*code, following) {
                    ("0", following) => {
                        (ink_dark, paper_dark) = (!dark_background, dark_background);
                        following
                    }
                    ("38", ["5", colour, following @ ..]) => {
                        ink_dark = fixed_colour_is_dark(colour);
                        following
                    }
                    ("48", ["5", colour, following @ ..]) => {
                        paper_dark = fixed_colour_is_dark(colour);
                        following
                    }
                    other => panic!("a colour this scanner cannot show: {other:?}"),
                };
            }
            rest = after;
            continue;
        }
        let (top, bottom) = match character {
            ' ' => (false, false),
            '▀' => (true, false),
            '▄' => (false, true),
            '█' => (true, true),
            other => panic!("not part of a code: {other:?} in {line:?}"),
        };
        let colour = |inked| if inked { ink_dark } else { paper_dark };
        cells.push((colour(top), colour(bottom)));
        rest = &rest[character.len_utf8()..];
    }
    cells
}

/// Colours 16 and 231 are the black and white of the 256-colour cube, which
/// no theme restyles.
fn fixed_colour_is_dark(colour: &str) -> bool {
    match colour {
        "16" => true,
        "231" => false,
        other => panic!("a colour a theme may restyle: {other}"),
    }
}
