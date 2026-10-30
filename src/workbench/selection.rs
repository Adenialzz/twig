use super::*;
use crate::mouse_selection::word_class;

/// 保存拖选开始时的历史与画面；后台 tmux 状态继续更新。
pub(super) struct Selection {
    pub pane: String,
    pub rect: Rect,
    cells: Vec<Vec<Cell>>,
    start: (usize, u16),
    end: (usize, u16),
    pub dragging: bool,
    moved: bool,
    clicks: u8,
    anchor: (usize, u16),
    view_top: usize,
}

impl Selection {
    pub fn new(pane: &Pane, x: u16, y: u16) -> Self {
        let point = (usize::from(y - pane.rect.y), x - pane.rect.x);
        Self {
            pane: pane.id.clone(),
            rect: pane.rect,
            cells: (0..pane.rect.height)
                .map(|y| {
                    (0..pane.rect.width)
                        .map(|x| pane.parser.cell(y, x).clone())
                        .collect()
                })
                .collect(),
            start: point,
            end: point,
            dragging: true,
            moved: false,
            clicks: 1,
            anchor: point,
            view_top: 0,
        }
    }

    pub fn capture_history(&mut self, client: &mut Client, offset: usize) -> Result<()> {
        let rows = client.command(&format!("capture-pane -p -e -N -S - -E - -t {}", self.pane))?;
        let mut parser = TerminalState::new(1, self.rect.width, 0);
        let mut cells = Vec::with_capacity(rows.len());
        for row in rows {
            parser.process(b"\r\x1b[2K");
            parser.process(&row);
            cells.push(
                (0..self.rect.width)
                    .map(|x| parser.cell(0, x).clone())
                    .collect(),
            );
        }
        let bottom = cells.len().saturating_sub(usize::from(self.rect.height));
        self.view_top = bottom.saturating_sub(offset);
        ensure!(
            cells.len() >= usize::from(self.rect.height),
            "Pane 历史快照缺少屏幕行"
        );
        self.cells = cells;
        self.start.0 += self.view_top;
        self.end.0 += self.view_top;
        self.anchor.0 += self.view_top;
        Ok(())
    }

    pub fn scroll(&mut self, up: bool, x: u16, y: u16) {
        self.view_top = if up {
            self.view_top.saturating_sub(3)
        } else {
            (self.view_top + 3).min(self.cells.len() - usize::from(self.rect.height))
        };
        self.update(x, y);
    }

    pub fn scroll_offset(&self) -> usize {
        self.cells.len() - usize::from(self.rect.height) - self.view_top
    }

    pub fn select_click(&mut self, clicks: u8) {
        self.clicks = clicks;
        if clicks > 1 {
            (self.start, self.end) = self.span(self.anchor);
            self.moved = true;
        }
    }

    fn span(&self, (y, mut x): (usize, u16)) -> ((usize, u16), (usize, u16)) {
        if self.clicks == 3 {
            return ((y, 0), (y, self.rect.width - 1));
        }
        if self
            .history_cell(y, x)
            .flags
            .contains(Flags::WIDE_CHAR_SPACER)
        {
            x = x.saturating_sub(1);
        }
        let class = word_class(self.history_cell(y, x).c);
        let mut left = x;
        let mut right = x;
        while left > 0 {
            let mut previous = left - 1;
            if self
                .history_cell(y, previous)
                .flags
                .contains(Flags::WIDE_CHAR_SPACER)
            {
                previous = previous.saturating_sub(1);
            }
            if class == 0 || word_class(self.history_cell(y, previous).c) != class {
                break;
            }
            left = previous;
        }
        while right + 1 < self.rect.width {
            let next = right + 1;
            if !self
                .history_cell(y, next)
                .flags
                .contains(Flags::WIDE_CHAR_SPACER)
                && (class == 0 || word_class(self.history_cell(y, next).c) != class)
            {
                break;
            }
            right = next;
        }
        ((y, left), (y, right))
    }

    pub fn update(&mut self, x: u16, y: u16) {
        let point = (
            self.view_top + usize::from(y.saturating_sub(self.rect.y).min(self.rect.height - 1)),
            x.saturating_sub(self.rect.x).min(self.rect.width - 1),
        );
        if self.clicks > 1 {
            let anchor = self.span(self.anchor);
            let target = self.span(point);
            self.start = anchor.0.min(target.0);
            self.end = anchor.1.max(target.1);
            return;
        }
        self.end = point;
        self.moved |= self.end != self.start;
    }

    pub fn cell(&self, y: u16, x: u16) -> &Cell {
        self.history_cell(self.view_top + usize::from(y), x)
    }

    fn history_cell(&self, y: usize, x: u16) -> &Cell {
        &self.cells[y][usize::from(x)]
    }

    pub fn contains(&self, y: u16, x: u16) -> bool {
        self.contains_history(self.view_top + usize::from(y), x)
    }

    fn contains_history(&self, y: usize, x: u16) -> bool {
        let start = self.start.min(self.end);
        let end = self.start.max(self.end);
        // 点击宽字符右半边时也选中整个字符。
        let left = if self
            .history_cell(y, x)
            .flags
            .contains(Flags::WIDE_CHAR_SPACER)
        {
            x.saturating_sub(1)
        } else {
            x
        };
        let right = if self.history_cell(y, left).flags.contains(Flags::WIDE_CHAR) {
            left + 1
        } else {
            left
        };
        self.moved && (y, right) >= start && (y, left) <= end
    }

    pub fn text(&self) -> String {
        if !self.moved {
            return String::new();
        }
        let start = self.start.min(self.end);
        let end = self.start.max(self.end);
        (start.0..=end.0)
            .map(|y| {
                let mut line = String::new();
                for x in 0..self.rect.width {
                    let cell = self.history_cell(y, x);
                    if self.contains_history(y, x) && !cell.flags.contains(Flags::WIDE_CHAR_SPACER)
                    {
                        line.push(cell.c);
                        if let Some(extra) = cell.zerowidth() {
                            line.extend(extra);
                        }
                    }
                }
                line.trim_end_matches(' ').to_owned()
            })
            .collect::<Vec<_>>()
            .join("\n")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn scrolling_preserves_anchor_and_clamps_frozen_history() {
        let pane = Pane {
            id: "%1".into(),
            rect: Rect::new(10, 3, 8, 2),
            active: true,
            parser: TerminalState::new(2, 8, 0),
            cursor: None,
            cursor_style: crossterm::cursor::SetCursorStyle::DefaultUserShape,
            scroll_offset: 0,
            title: String::new(),
        };
        let mut selected = Selection::new(&pane, 10, 3);
        selected.cells = (0..10)
            .map(|i| {
                let mut parser = TerminalState::new(1, 8, 0);
                parser.process(format!("中文{i}").as_bytes());
                (0..8).map(|x| parser.cell(0, x).clone()).collect()
            })
            .collect();
        selected.view_top = 8;
        selected.anchor = (8, 1);
        selected.start = selected.anchor;
        selected.end = selected.anchor;
        selected.select_click(2);
        selected.scroll(true, 11, 3);
        assert_eq!(selected.text(), "中文5\n中文6\n中文7\n中文");
        assert_eq!(selected.cell(0, 4).c, '5');
        assert!(selected.contains(0, 0));
        selected.scroll(false, 11, 3);
        assert_eq!(selected.text(), "中文");
        for _ in 0..5 {
            selected.scroll(true, 11, 3);
        }
        assert_eq!(selected.cell(0, 4).c, '0');
        assert!(selected.text().starts_with("中文0\n中文1"));
        for _ in 0..5 {
            selected.scroll(false, 17, 4);
        }
        assert_eq!(selected.text(), "中文8\n中文9");
        assert_eq!(selected.cell(1, 4).c, '9');
    }

    #[test]
    fn multiple_clicks_select_words_and_screen_lines() {
        let mut pane = Pane {
            id: "%1".into(),
            rect: Rect::new(10, 3, 24, 2),
            active: true,
            parser: TerminalState::new(2, 24, 0),
            cursor: None,
            cursor_style: crossterm::cursor::SetCursorStyle::DefaultUserShape,
            scroll_offset: 0,
            title: String::new(),
        };
        pane.parser
            .process("foo_42 中文e\u{301}. tail\r\nnext".as_bytes());
        for (x, expected) in [(12, "foo_42"), (18, "中文"), (21, "e\u{301}"), (22, ".")] {
            let mut selected = Selection::new(&pane, x, 3);
            selected.select_click(2);
            selected.update(x, 3);
            assert_eq!(selected.text(), expected);
        }
        let mut selected = Selection::new(&pane, 18, 3);
        selected.select_click(3);
        selected.update(18, 3);
        assert_eq!(selected.text(), "foo_42 中文e\u{301}. tail");
        pane.parser.process(b"\x1b[2Jchanged");
        selected.update(100, 100);
        assert_eq!(selected.text(), "foo_42 中文e\u{301}. tail\nnext");
        assert!(selected.contains(0, 0));
        assert!(selected.contains(1, 23));
    }

    #[test]
    fn selection_is_frozen_unicode_plain_text_and_clamped_to_pane() {
        let mut pane = Pane {
            id: "%1".into(),
            rect: Rect::new(10, 3, 8, 2),
            active: true,
            parser: TerminalState::new(2, 8, 0),
            cursor: None,
            cursor_style: crossterm::cursor::SetCursorStyle::DefaultUserShape,
            scroll_offset: 0,
            title: String::new(),
        };
        pane.parser
            .process("\x1b[31m中文e\u{301}\r\nnext".as_bytes());
        let mut selection = Selection::new(&pane, 11, 3);
        assert_eq!(selection.text(), "");
        selection.update(80, 20);
        pane.parser.process(b"\x1b[2Jchanged");
        assert_eq!(selection.text(), "中文e\u{301}\nnext");
        assert!(selection.contains(0, 0));
        selection.start = (1, 7);
        selection.update(11, 3);
        assert_eq!(selection.text(), "中文e\u{301}\nnext");
    }
}
