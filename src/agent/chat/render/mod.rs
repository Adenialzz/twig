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

pub(super) fn markdown(text: &str, width: usize) -> Vec<Line<'static>> {
    markdown_render::render_markdown_text_with_width_and_cwd(text, Some(width.max(1)), None).lines
}

pub(super) fn plain(text: &str, width: usize, style: Style) -> Vec<Line<'static>> {
    text.lines()
        .flat_map(|text| {
            let line = Line::styled(text.to_owned(), style);
            wrapping::word_wrap_line(&line, width.max(1))
                .iter()
                .map(line_utils::line_to_static)
                .collect::<Vec<_>>()
        })
        .collect()
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
