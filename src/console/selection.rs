use ratatui::{buffer::Buffer, layout::Rect, style::Modifier, text::Line};

/// 保存起始区域的已显示字符；数据采集继续，直到用户清除选区。
pub(super) struct Selection {
    buffer: Buffer,
    area: Rect,
    start: (u16, u16),
    end: (u16, u16),
    pub dragging: bool,
    moved: bool,
}

impl Selection {
    pub fn fits(&self, area: Rect) -> bool {
        self.buffer.area == area
    }
    pub fn new(buffer: &Buffer, area: Rect, x: u16, y: u16) -> Self {
        Self {
            buffer: buffer.clone(),
            area,
            start: (y, x),
            end: (y, x),
            dragging: true,
            moved: false,
        }
    }

    pub fn update(&mut self, x: u16, y: u16) {
        self.end = (
            y.clamp(self.area.y, self.area.bottom() - 1),
            x.clamp(self.area.x, self.area.right() - 1),
        );
        self.moved |= self.start != self.end;
    }

    fn selected(&self, x: u16, y: u16, width: u16) -> bool {
        self.moved
            && (y, x + width - 1) >= self.start.min(self.end)
            && (y, x) <= self.start.max(self.end)
    }

    pub fn text(&self) -> String {
        if !self.moved {
            return String::new();
        }
        let mut lines = Vec::new();
        for y in self.start.min(self.end).0..=self.start.max(self.end).0 {
            let mut line = String::new();
            let mut x = self.area.x;
            while x < self.area.right() {
                let symbol = self.buffer[(x, y)].symbol();
                let width = Line::from(symbol).width().max(1) as u16;
                if self.selected(x, y, width) {
                    line.push_str(symbol);
                }
                x += width;
            }
            lines.push(line.trim_end_matches(' ').to_owned());
        }
        lines.join("\n")
    }

    pub fn draw(&self, target: &mut Buffer) {
        for y in self.area.y..self.area.bottom() {
            let mut x = self.area.x;
            while x < self.area.right() {
                let cell = &self.buffer[(x, y)];
                let width = Line::from(cell.symbol()).width().max(1) as u16;
                for offset in 0..width.min(self.area.right() - x) {
                    target[(x + offset, y)] = self.buffer[(x + offset, y)].clone();
                    if self.selected(x, y, width) {
                        target[(x + offset, y)].modifier.toggle(Modifier::REVERSED);
                    }
                }
                x += width;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ratatui::style::Style;

    #[test]
    fn frozen_region_clamps_reverse_selection_and_keeps_wide_characters() {
        let mut buffer = Buffer::empty(Rect::new(0, 0, 20, 5));
        buffer.set_string(2, 1, "中文e\u{301}", Style::default());
        buffer.set_string(2, 2, "next", Style::default());
        let mut selection = Selection::new(&buffer, Rect::new(2, 1, 8, 2), 9, 2);
        assert_eq!(selection.text(), "");
        selection.update(3, 0);
        assert_eq!(selection.text(), "中文e\u{301}\nnext");
        buffer.set_string(2, 1, "changed", Style::default());
        selection.draw(&mut buffer);
        assert_eq!(buffer[(2, 1)].symbol(), "中");
        assert!(buffer[(2, 1)].modifier.contains(Modifier::REVERSED));
        assert_eq!(buffer[(10, 1)].symbol(), " ");
    }
}
