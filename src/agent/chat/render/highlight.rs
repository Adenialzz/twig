// Adapted from OpenAI Codex f1affbac5e5164b2bae825e9b39e9868bc4e0be2.
// Copyright 2025 OpenAI. Apache-2.0; see third_party/codex/NOTICE.
// Twig uses one bundled theme; no Codex configuration or theme discovery.
use ratatui::{
    style::{Color, Modifier, Style},
    text::{Line, Span},
};
use std::sync::LazyLock;
use syntect::{
    easy::HighlightLines,
    highlighting::{FontStyle, ThemeSet},
    parsing::SyntaxSet,
    util::LinesWithEndings,
};

static SYNTAX: LazyLock<SyntaxSet> = LazyLock::new(SyntaxSet::load_defaults_newlines);
static THEMES: LazyLock<ThemeSet> = LazyLock::new(ThemeSet::load_defaults);

pub(super) fn foreground_style_for_scopes(_: &[&str]) -> Option<Style> {
    Some(Style::default().fg(Color::Cyan))
}

pub(super) fn highlight_code_to_lines(code: &str, lang: &str) -> Vec<Line<'static>> {
    let syntax = SYNTAX.find_syntax_by_token(lang.split_whitespace().next().unwrap_or_default());
    if let Some(syntax) = syntax.filter(|_| code.len() <= 512_000 && code.lines().count() <= 10_000)
    {
        let mut highlighter = HighlightLines::new(syntax, &THEMES.themes["base16-ocean.dark"]);
        let rendered: Result<Vec<_>, _> = LinesWithEndings::from(code)
            .map(|line| {
                highlighter.highlight_line(line, &SYNTAX).map(|ranges| {
                    Line::from(
                        ranges
                            .into_iter()
                            .map(|(style, text)| {
                                let fg = style.foreground;
                                let mut output = Style::default().fg(Color::Rgb(fg.r, fg.g, fg.b));
                                for (font, modifier) in [
                                    (FontStyle::BOLD, Modifier::BOLD),
                                    (FontStyle::ITALIC, Modifier::ITALIC),
                                    (FontStyle::UNDERLINE, Modifier::UNDERLINED),
                                ] {
                                    if style.font_style.contains(font) {
                                        output = output.add_modifier(modifier);
                                    }
                                }
                                Span::styled(text.trim_end_matches(['\r', '\n']).to_owned(), output)
                            })
                            .collect::<Vec<_>>(),
                    )
                })
            })
            .collect();
        if let Ok(lines) = rendered {
            return lines;
        }
    }
    code.lines()
        .map(|line| Line::from(line.to_owned()))
        .collect()
}
