use super::*;

/// 只保存用户开始拖选时看到的画面；后台 tmux 状态继续更新。
pub(super) struct Selection {
    pub pane: String,
    pub rect: Rect,
    cells: Vec<Vec<Cell>>,
    start: (u16, u16),
    end: (u16, u16),
    pub dragging: bool,
    moved: bool,
}

impl Selection {
    pub fn new(pane: &Pane, x: u16, y: u16) -> Self {
        let point = (y - pane.rect.y, x - pane.rect.x);
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
        }
    }

    pub fn update(&mut self, x: u16, y: u16) {
        self.end = (
            y.saturating_sub(self.rect.y).min(self.rect.height - 1),
            x.saturating_sub(self.rect.x).min(self.rect.width - 1),
        );
        self.moved |= self.end != self.start;
    }

    pub fn cell(&self, y: u16, x: u16) -> &Cell {
        &self.cells[usize::from(y)][usize::from(x)]
    }

    pub fn contains(&self, y: u16, x: u16) -> bool {
        let start = self.start.min(self.end);
        let end = self.start.max(self.end);
        // 点击宽字符右半边时也选中整个字符。
        let left = if self.cell(y, x).flags.contains(Flags::WIDE_CHAR_SPACER) {
            x.saturating_sub(1)
        } else {
            x
        };
        let right = if self.cell(y, left).flags.contains(Flags::WIDE_CHAR) {
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
                    let cell = self.cell(y, x);
                    if self.contains(y, x) && !cell.flags.contains(Flags::WIDE_CHAR_SPACER) {
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
