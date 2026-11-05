//! Agent 正文渲染；来源与本地适配见 third_party/codex/README.md。
mod highlight;
mod line_utils;
mod location;
mod markdown_render;
mod markdown_text_merge;
mod terminal_hyperlinks;
mod wrapping;

use ratatui::{
    style::{Color, Style},
    text::Line,
};

mod style {
    use super::*;
    pub(super) fn table_separator_style() -> Style {
        Style::default().fg(Color::DarkGray)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ratatui::style::Modifier;

    #[test]
    fn rich_text_preserves_content_and_styles() {
        let lines = markdown(
            "## 标题\n\n**重点** 与 `inline`，*斜体*。\n\n> 引用\n\n- 第一项\n- 第二项\n\n[说明](https://example.com)",
            40,
        );
        let text = lines
            .iter()
            .map(ToString::to_string)
            .collect::<Vec<_>>()
            .join("\n");
        assert!(text.contains("重点") && text.contains("inline") && text.contains("引用"));
        assert!(!text.contains("**") && !text.contains("[说明]("));
        assert!(
            lines
                .iter()
                .flat_map(|line| &line.spans)
                .any(|span| span.content.contains("重点")
                    && span.style.add_modifier.contains(Modifier::BOLD))
        );
        assert!(
            lines
                .iter()
                .flat_map(|line| &line.spans)
                .any(|span| span.content.contains("斜体")
                    && span.style.add_modifier.contains(Modifier::ITALIC))
        );
    }

    #[test]
    fn chinese_tables_remain_readable_at_multiple_widths() {
        let source = "| 文件 | 结果 |\n| --- | --- |\n| src/agent/chat/view.rs | 中文渲染与工具摘要已经更新 |\n| Cargo.toml | 依赖更新 |";
        for width in [24, 40, 80] {
            let lines = markdown(source, width);
            assert!(
                lines.iter().all(|line| line.width() <= width),
                "width {width}: {lines:?}"
            );
            let text = lines.iter().map(ToString::to_string).collect::<String>();
            assert!(text.contains("Cargo.toml"));
            assert!(text.contains("依赖更新"));
        }
    }

    #[test]
    fn code_highlight_unknown_language_and_partial_fences_keep_text() {
        let lines = markdown("```rust\nfn main() {\n    println!(\"你好\");\n}\n```", 60);
        assert!(
            lines
                .iter()
                .flat_map(|line| &line.spans)
                .any(|span| matches!(span.style.fg, Some(Color::Rgb(..))))
        );
        for source in ["```unknown\nraw_text\n```", "```rust\nlet unfinished = 1;"] {
            let text = markdown(source, 60)
                .iter()
                .map(ToString::to_string)
                .collect::<String>();
            assert!(text.contains(if source.contains("unknown") {
                "raw_text"
            } else {
                "unfinished"
            }));
        }
    }

    #[test]
    fn plain_output_never_interprets_markdown() {
        let text = "**literal**\n```rust\n# error";
        assert_eq!(
            plain(text, 80, Style::default())
                .iter()
                .map(ToString::to_string)
                .collect::<Vec<_>>()
                .join("\n"),
            text
        );
    }
}

pub(super) use terminal_hyperlinks::HyperlinkLine as CopyLine;

// 行正文直接来自排版器的源文本切片；只跳过排版丢弃的空格与 Tab，保留它们作为拼接分隔。
pub(super) fn copy_wrapped(
    source: &Line<'_>,
    lines: &mut [CopyLine],
    first_indent: usize,
    indent: usize,
) {
    let source = source.to_string();
    let mut cursor = 0;
    let mut previous: Option<usize> = None;
    for i in 0..lines.len() {
        let text = lines[i].line.to_string();
        let prefix = if i == 0 { first_indent } else { indent };
        let mut columns = 0;
        let body = text.trim_start_matches(|c: char| {
            if columns < prefix {
                columns += unicode_width::UnicodeWidthChar::width(c).unwrap_or(0);
                true
            } else {
                false
            }
        });
        let mut start = cursor;
        while !source[start..].starts_with(body) && source[start..].starts_with([' ', '\t']) {
            start += 1;
        }
        assert!(
            source[start..].starts_with(body),
            "排版行必须对应源文本切片"
        );
        if let Some(previous) = previous {
            lines[previous].copy.join = Some(source[cursor..start].to_owned());
        }
        lines[i].copy.skip = prefix;
        lines[i].copy.width = lines[i].line.width();
        cursor = start + body.len();
        previous = Some(i);
    }
}

pub(super) fn plain(text: &str, width: usize, style: Style) -> Vec<CopyLine> {
    text.split('\n')
        .flat_map(|text| {
            let source = Line::styled(text.to_owned(), style);
            let mut lines: Vec<_> = wrapping::word_wrap_line(&source, width.max(1))
                .iter()
                .map(|line| CopyLine::new(line_utils::line_to_static(line)))
                .collect();
            copy_wrapped(&source, &mut lines, 0, 0);
            lines.into_iter().map(display_tabs).collect::<Vec<_>>()
        })
        .collect()
}

pub(super) fn markdown(text: &str, width: usize) -> Vec<CopyLine> {
    markdown_render::render_markdown_lines_with_width_and_cwd(text, Some(width.max(1)), None)
        .into_iter()
        .flat_map(|line| fit_line(line, width))
        .collect()
}

pub(super) fn fit_line(line: CopyLine, width: usize) -> Vec<CopyLine> {
    if line.width() <= width {
        return vec![display_tabs(line)];
    }
    let mut rows: Vec<_> = wrapping::word_wrap_line(&line.line, width.max(1))
        .iter()
        .map(|row| CopyLine::new(line_utils::line_to_static(row)))
        .collect();
    copy_wrapped(&line.line, &mut rows, 0, 0);
    if let Some(first) = rows.first_mut() {
        first.copy.skip = line.copy.skip;
    }
    if let Some(last) = rows.last_mut() {
        last.copy.join = line.copy.join;
    }
    rows.into_iter().map(display_tabs).collect()
}

// 原始 Tab 不能直接写入终端；显示为一列空白，复制时由选区还原。
fn display_tabs(mut line: CopyLine) -> CopyLine {
    let text = line.line.to_string();
    for (byte, _) in text.match_indices('\t') {
        line.copy.tabs.push(Line::from(&text[..byte]).width());
    }
    for span in &mut line.line.spans {
        if span.content.contains('\t') {
            span.content = span.content.replace('\t', " ").into();
        }
    }
    line
}

#[cfg(test)]
mod copy_tests {
    use super::*;
    use crate::console::selection::Selection;
    use ratatui::{
        buffer::Buffer,
        layout::Rect,
        text::Text,
        widgets::{Paragraph, Widget},
    };

    fn copy(lines: Vec<CopyLine>, width: u16) -> String {
        let area = Rect::new(0, 0, width, lines.len() as u16);
        let mut buffer = Buffer::empty(area);
        let metadata = lines.iter().map(|line| line.copy.clone()).collect();
        Paragraph::new(Text::from(
            lines.into_iter().map(|line| line.line).collect::<Vec<_>>(),
        ))
        .render(area, &mut buffer);
        let mut selection = Selection::new(&buffer, area, 0, 0);
        selection.set_copy_rows(metadata);
        selection.update(width - 1, area.height - 1);
        selection.text()
    }

    #[test]
    fn tabs_do_not_truncate_rendered_or_copied_text() {
        for width in [7, 24, 74, 80] {
            for text in [
                format!("{}\tdef xyz end", "a".repeat(71)),
                "\t中文\t\tcode with spaces\n\tsecond\tend".into(),
                "prefix\thttps://example.com/very/long/path\tend".into(),
            ] {
                let lines = plain(&text, width, Style::default());
                assert!(
                    lines
                        .iter()
                        .all(|line| line.width() <= width && !line.to_string().contains('\t'))
                );
                assert_eq!(copy(lines, width as u16), text);
                for lang in ["rust", "unknown"] {
                    let lines = markdown(&format!("```{lang}\n{text}\n```"), width);
                    assert!(
                        lines
                            .iter()
                            .all(|line| line.width() <= width && !line.to_string().contains('\t'))
                    );
                    assert!(copy(lines, width as u16).contains(&text));
                }
            }
        }
    }

    #[test]
    fn plain_selection_restores_spaces_unicode_and_real_newlines() {
        for width in [7, 12, 24, 80] {
            for text in [
                "hello world this is a sentence",
                "中文自动折行继续连接与英文 mixed words",
                "abcde  next words",
                "https://example.com/very/long/path",
                "    code_indent_and_long_name();\n\n    second();",
                "组合e\u{301}继续中文",
            ] {
                assert_eq!(
                    copy(plain(text, width, Style::default()), width as u16),
                    text,
                    "width={width}"
                );
            }
        }
    }

    #[test]
    fn markdown_selection_keeps_blocks_and_joins_only_wrapped_rows() {
        for width in [12, 24, 60] {
            let text = "Hello **world** with 中文 and more words.\n\nAnother paragraph.";
            assert_eq!(
                copy(markdown(text, width), width as u16).trim(),
                "Hello world with 中文 and more words.\n\nAnother paragraph."
            );
            let code = "    long_variable_name = 123456789;\n\n    second_line();";
            let result = copy(
                markdown(&format!("```rust\n{code}\n```"), width),
                width as u16,
            );
            assert!(result.contains(code), "{result:?}");
            let result = copy(
                markdown("- hello world with lots of words\n- second item", width),
                width as u16,
            );
            assert!(
                result.contains("hello world with lots of words"),
                "{result:?}"
            );
            assert!(result.contains("\n"));
            let result = copy(
                markdown("- intro https://example.com/very/long/path and end", width),
                width as u16,
            );
            assert!(
                result.contains("intro https://example.com/very/long/path and end"),
                "{result:?}"
            );
        }
    }
}
