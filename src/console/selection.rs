use ratatui::{buffer::Buffer, layout::Rect, style::Modifier, text::Line};
use twig_cli::mouse_selection::word_class;

/// 保存起始区域的已显示字符；数据采集继续，直到用户清除选区。
pub(crate) struct Selection {
    buffer: Buffer,
    area: Rect,
    screen: Rect,
    start: (u16, u16),
    end: (u16, u16),
    pub dragging: bool,
    moved: bool,
    clicks: u8,
    anchor: (u16, u16),
}

impl Selection {
    pub fn fits(&self, area: Rect) -> bool {
        self.screen == area
    }
    pub fn new(buffer: &Buffer, area: Rect, x: u16, y: u16) -> Self {
        let mut region = Buffer::empty(area);
        for y in area.y..area.bottom() {
            for x in area.x..area.right() {
                region[(x, y)] = buffer[(x, y)].clone();
            }
        }
        Self {
            buffer: region,
            screen: buffer.area,
            area,
            start: (y, x),
            end: (y, x),
            dragging: true,
            moved: false,
            clicks: 1,
            anchor: (y, x),
        }
    }

    pub fn select_click(&mut self, clicks: u8) {
        self.clicks = clicks;
        if clicks > 1 {
            (self.start, self.end) = self.span(self.anchor);
            self.moved = true;
        }
    }

    fn span(&self, (y, x): (u16, u16)) -> ((u16, u16), (u16, u16)) {
        if self.clicks == 3 {
            return ((y, self.area.x), (y, self.area.right() - 1));
        }
        let mut cells = Vec::new();
        let mut column = self.area.x;
        while column < self.area.right() {
            let symbol = self.buffer[(column, y)].symbol();
            let width = Line::from(symbol).width().max(1) as u16;
            cells.push((
                column,
                (column + width - 1).min(self.area.right() - 1),
                word_class(symbol.chars().next().unwrap_or(' ')),
            ));
            column += width;
        }
        let index = cells
            .iter()
            .position(|&(left, right, _)| x >= left && x <= right)
            .unwrap();
        let class = cells[index].2;
        let mut left = index;
        let mut right = index;
        if class != 0 {
            while left > 0 && cells[left - 1].2 == class {
                left -= 1;
            }
            while right + 1 < cells.len() && cells[right + 1].2 == class {
                right += 1;
            }
        }
        ((y, cells[left].0), (y, cells[right].1))
    }

    pub fn update(&mut self, x: u16, y: u16) {
        let point = (
            y.clamp(self.area.y, self.area.bottom() - 1),
            x.clamp(self.area.x, self.area.right() - 1),
        );
        if self.clicks > 1 {
            let anchor = self.span(self.anchor);
            let target = self.span(point);
            self.start = anchor.0.min(target.0);
            self.end = anchor.1.max(target.1);
            return;
        }
        self.end = point;
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
    fn multiple_clicks_respect_unicode_and_region_edges() {
        let mut buffer = Buffer::empty(Rect::new(0, 0, 40, 5));
        let area = Rect::new(2, 1, 24, 2);
        buffer.set_string(2, 1, "foo_42 中文e\u{301}. tail", Style::default());
        buffer.set_string(2, 2, "next", Style::default());
        buffer.set_string(26, 1, "outside", Style::default());
        for (x, expected) in [(4, "foo_42"), (10, "中文"), (13, "e\u{301}"), (14, ".")] {
            let mut selection = Selection::new(&buffer, area, x, 1);
            selection.select_click(2);
            selection.update(x, 1);
            assert_eq!(selection.text(), expected);
        }
        let mut selection = Selection::new(&buffer, area, 10, 1);
        selection.select_click(2);
        selection.update(0, 0);
        assert_eq!(selection.text(), "foo_42 中文");
        let mut selection = Selection::new(&buffer, area, 10, 1);
        selection.select_click(3);
        selection.update(10, 1);
        assert_eq!(selection.text(), "foo_42 中文e\u{301}. tail");
        buffer.set_string(2, 1, "changed", Style::default());
        selection.update(39, 4);
        assert_eq!(selection.text(), "foo_42 中文e\u{301}. tail\nnext");
        selection.draw(&mut buffer);
        assert!(buffer[(2, 1)].modifier.contains(Modifier::REVERSED));
        assert!(buffer[(25, 2)].modifier.contains(Modifier::REVERSED));
        assert!(!buffer[(26, 1)].modifier.contains(Modifier::REVERSED));
        assert!(!selection.fits(Rect::new(0, 0, 20, 5)));
    }

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
