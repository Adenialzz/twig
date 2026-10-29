//! Ratatui 的 Buffer 保留纯文本；在完成所有浮层绘制后补写 OSC 8。
//! 每次先用当前画面重写旧链接位置，避免同标签换目标或弹窗覆盖时残留链接。
use crossterm::{
    cursor::{RestorePosition, SavePosition},
    queue,
};
use ratatui::{
    backend::{Backend, CrosstermBackend},
    buffer::Buffer,
    layout::Rect,
};
use std::io::{self, Write};

pub const LABEL: &str = "用 Zed 打开 ↗";

pub struct Link {
    pub area: Rect,
    pub url: String,
}

#[derive(Default)]
pub struct Painter {
    previous: Option<Rect>,
}

impl Painter {
    pub fn draw(
        &mut self,
        output: &mut impl Write,
        buffer: &Buffer,
        link: Option<&Link>,
    ) -> io::Result<()> {
        let link = link.filter(|link| {
            let area = link.area;
            let mut expected = Buffer::empty(area);
            expected.set_string(area.x, area.y, LABEL, ratatui::style::Style::default());
            buffer.area.contains((area.x, area.y).into())
                && buffer.area.contains((area.right() - 1, area.y).into())
                && (area.x..area.right())
                    .all(|x| buffer[(x, area.y)].symbol() == expected[(x, area.y)].symbol())
        });
        if self.previous.is_none() && link.is_none() {
            return Ok(());
        }
        queue!(output, SavePosition)?;
        write!(output, "\x1b]8;;\x1b\\")?;
        if let Some(previous) = self.previous.take() {
            paint(output, buffer, previous.intersection(buffer.area))?;
        }
        if let Some(link) = link {
            write!(output, "\x1b]8;;{}\x1b\\", link.url)?;
            paint(output, buffer, link.area)?;
            write!(output, "\x1b]8;;\x1b\\")?;
            self.previous = Some(link.area);
        }
        queue!(output, RestorePosition)?;
        output.flush()
    }
}

fn paint(output: &mut impl Write, buffer: &Buffer, area: Rect) -> io::Result<()> {
    // 跳过宽字符的续格，不能用空格覆盖中文的后半格。
    let mut cells = Vec::new();
    for y in area.y..area.bottom() {
        let mut x = buffer.area.x;
        while x < area.right() {
            let cell = &buffer[(x, y)];
            let width = unicode_width::UnicodeWidthStr::width(cell.symbol()).max(1) as u16;
            if x + width > area.x {
                cells.push((x, y, cell));
            }
            x += width;
        }
    }
    CrosstermBackend::new(output).draw(cells.into_iter())
}

#[cfg(test)]
mod tests {
    use super::*;
    use ratatui::style::Style;

    #[test]
    fn links_follow_targets_and_disappear_when_covered_or_resized() {
        let mut buffer = Buffer::empty(Rect::new(0, 0, 40, 5));
        buffer.set_string(2, 1, LABEL, Style::default());
        let mut link = Link {
            area: Rect::new(2, 1, unicode_width::UnicodeWidthStr::width(LABEL) as u16, 1),
            url: "zed://ssh/host/first".into(),
        };
        let mut painter = Painter::default();
        let mut output = Vec::new();
        painter.draw(&mut output, &buffer, Some(&link)).unwrap();
        assert!(String::from_utf8_lossy(&output).contains("\x1b]8;;zed://ssh/host/first\x1b\\"));
        assert!(
            !buffer
                .content
                .iter()
                .any(|cell| cell.symbol().contains('\x1b'))
        );
        output.clear();
        link.url = "zed://ssh/host/second".into();
        painter.draw(&mut output, &buffer, Some(&link)).unwrap();
        let text = String::from_utf8_lossy(&output);
        assert!(text.starts_with("\x1b7\x1b]8;;\x1b\\"));
        assert!(text.contains("zed://ssh/host/second"));
        assert!(!text.contains("host/first"));
        buffer.set_string(2, 1, "弹窗遮挡", Style::default());
        output.clear();
        painter.draw(&mut output, &buffer, Some(&link)).unwrap();
        assert!(!String::from_utf8_lossy(&output).contains("zed://"));
        assert!(painter.previous.is_none());
        painter.previous = Some(link.area);
        output.clear();
        painter
            .draw(&mut output, &Buffer::empty(Rect::new(0, 0, 3, 1)), None)
            .unwrap();
        assert!(painter.previous.is_none());
    }
}
