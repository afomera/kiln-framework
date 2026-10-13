//! Output lines (text with SGR color escapes) as styled, highlighted, wrapped spans.

use ratatui::{
    style::{Color, Modifier, Style},
    text::Span,
};
use unicode_width::UnicodeWidthChar;

/// `text`'s runs of characters and their styles. The runs' text, joined, is the text
/// without escapes ([`crate::dev::lines::strip`]).
pub fn parse(text: &str, base: Style) -> Vec<(Style, String)> {
    let mut runs: Vec<(Style, String)> = vec![];
    let mut style = base;
    let mut chars = text.chars().peekable();
    while let Some(c) = chars.next() {
        if c != '\x1b' {
            match runs.last_mut() {
                Some((s, run)) if *s == style => run.push(c),
                _ => runs.push((style, c.to_string())),
            }
            continue;
        }
        let mut params = String::new();
        let mut end = None;
        for c in chars.by_ref() {
            if c.is_ascii_alphabetic() {
                end = Some(c);
                break;
            }
            params.push(c);
        }
        if end == Some('m') {
            style = sgr(style, base, params.trim_start_matches('['));
        }
    }
    runs
}

fn sgr(mut style: Style, base: Style, params: &str) -> Style {
    let codes: Vec<u16> = params.split([';', ':']).map(|p| p.parse().unwrap_or(0)).collect();
    let mut i = 0;
    while i < codes.len() {
        let code = codes[i];
        match code {
            0 => style = base,
            1 => style = style.add_modifier(Modifier::BOLD),
            2 => style = style.add_modifier(Modifier::DIM),
            3 => style = style.add_modifier(Modifier::ITALIC),
            4 => style = style.add_modifier(Modifier::UNDERLINED),
            7 => style = style.add_modifier(Modifier::REVERSED),
            9 => style = style.add_modifier(Modifier::CROSSED_OUT),
            22 => style = style.remove_modifier(Modifier::BOLD | Modifier::DIM),
            23 => style = style.remove_modifier(Modifier::ITALIC),
            24 => style = style.remove_modifier(Modifier::UNDERLINED),
            27 => style = style.remove_modifier(Modifier::REVERSED),
            29 => style = style.remove_modifier(Modifier::CROSSED_OUT),
            30..=37 => style = style.fg(Color::Indexed((code - 30) as u8)),
            39 => style.fg = base.fg,
            40..=47 => style = style.bg(Color::Indexed((code - 40) as u8)),
            49 => style.bg = base.bg,
            90..=97 => style = style.fg(Color::Indexed((code - 90 + 8) as u8)),
            100..=107 => style = style.bg(Color::Indexed((code - 100 + 8) as u8)),
            38 | 48 => {
                let color = match codes.get(i + 1) {
                    Some(5) => {
                        let color = codes.get(i + 2).map(|&n| Color::Indexed(n as u8));
                        i += 2;
                        color
                    }
                    Some(2) => {
                        let rgb = (codes.get(i + 2), codes.get(i + 3), codes.get(i + 4));
                        i += 4;
                        match rgb {
                            (Some(&r), Some(&g), Some(&b)) => Some(Color::Rgb(r as u8, g as u8, b as u8)),
                            _ => None,
                        }
                    }
                    _ => None,
                };
                if let Some(color) = color {
                    style = if code == 38 { style.fg(color) } else { style.bg(color) };
                }
            }
            _ => {}
        }
        i += 1;
    }
    style
}

/// Byte ranges of `needle` (lowercase ASCII) in `haystack`, ignoring ASCII case.
pub fn find(haystack: &str, needle: &str) -> Vec<(usize, usize)> {
    let (hay, needle) = (haystack.as_bytes(), needle.as_bytes());
    if needle.is_empty() || needle.len() > hay.len() {
        return vec![];
    }
    let mut found = vec![];
    let mut i = 0;
    while i + needle.len() <= hay.len() {
        if hay[i..i + needle.len()].iter().zip(needle).all(|(a, b)| a.to_ascii_lowercase() == *b) {
            found.push((i, i + needle.len()));
            i += needle.len();
        } else {
            i += 1;
        }
    }
    found
}

/// Whether `haystack` contains `needle` (lowercase ASCII), ignoring ASCII case.
pub fn contains(haystack: &str, needle: &str) -> bool {
    needle.is_empty() || !find(haystack, needle).is_empty()
}

/// `runs` with the byte ranges `marks` (of the runs' joined text) in `mark`'s style.
pub fn highlight(runs: Vec<(Style, String)>, marks: &[(usize, usize)], mark: Style) -> Vec<(Style, String)> {
    if marks.is_empty() {
        return runs;
    }
    let mut out: Vec<(Style, String)> = vec![];
    let mut offset = 0;
    for (style, text) in runs {
        for c in text.chars() {
            let marked = marks.iter().any(|&(start, end)| offset >= start && offset < end);
            let style = if marked { style.patch(mark) } else { style };
            match out.last_mut() {
                Some((s, run)) if *s == style => run.push(c),
                _ => out.push((style, c.to_string())),
            }
            offset += c.len_utf8();
        }
    }
    out
}

/// `runs` split into rows at most `width` columns wide (at least one row).
pub fn wrap(runs: &[(Style, String)], width: usize) -> Vec<Vec<Span<'static>>> {
    let width = width.max(1);
    let mut rows: Vec<Vec<Span<'static>>> = vec![vec![]];
    let mut used = 0;
    for (style, text) in runs {
        let mut run = String::new();
        for c in text.chars() {
            let w = c.width().unwrap_or(0);
            if used + w > width && used > 0 {
                if !run.is_empty() {
                    rows.last_mut().expect("a row").push(Span::styled(std::mem::take(&mut run), *style));
                }
                rows.push(vec![]);
                used = 0;
            }
            run.push(c);
            used += w;
        }
        if !run.is_empty() {
            rows.last_mut().expect("a row").push(Span::styled(run, *style));
        }
    }
    rows
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::dev::lines::strip;

    #[test]
    fn parses_colors() {
        let text = "\x1b[1;32mok\x1b[0m done \x1b[38;5;208mx\x1b[39m";
        let runs = parse(text, Style::default());
        let joined: String = runs.iter().map(|(_, t)| t.as_str()).collect();
        assert_eq!(joined, strip(text));
        assert_eq!(runs[0].0, Style::default().fg(Color::Indexed(2)).add_modifier(Modifier::BOLD));
        assert_eq!(runs[1].0, Style::default());
        assert_eq!(runs[2].0.fg, Some(Color::Indexed(208)));
    }

    #[test]
    fn finds_ignoring_case_and_highlights() {
        assert_eq!(find("GET /Posts posts", "posts"), [(5, 10), (11, 16)]);
        assert!(contains("Error", "err"));
        let mark = Style::default().bg(Color::Yellow);
        let runs = highlight(vec![(Style::default(), "abcd".into())], &[(1, 3)], mark);
        assert_eq!(runs.len(), 3);
        assert_eq!(runs[1], (mark, "bc".into()));
    }

    #[test]
    fn wraps_at_the_width() {
        let rows = wrap(&[(Style::default(), "abcdefg".into())], 3);
        let text: Vec<String> = rows.iter().map(|r| r.iter().map(|s| s.content.to_string()).collect()).collect();
        assert_eq!(text, ["abc", "def", "g"]);
        assert_eq!(wrap(&[], 10).len(), 1);
    }
}
