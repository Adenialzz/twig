//! 各页面共用的连击计数与词边界规则。
use std::time::{Duration, Instant};

#[derive(Default)]
pub struct Clicks<T> {
    last: Option<(T, u16, u16, Instant, u8)>,
}

impl<T: Eq + Clone> Clicks<T> {
    pub fn reset(&mut self) {
        self.last = None;
    }

    pub fn next(&mut self, pane: &T, x: u16, y: u16, now: Instant) -> u8 {
        let count = self.last.as_ref().map_or(1, |(id, px, py, at, count)| {
            if id == pane
                && *px == x
                && *py == y
                && now.duration_since(*at) <= Duration::from_millis(400)
            {
                count % 3 + 1
            } else {
                1
            }
        });
        self.last = Some((pane.clone(), x, y, now, count));
        count
    }
}

pub fn word_class(c: char) -> u8 {
    match c {
        '\u{3400}'..='\u{4dbf}'
        | '\u{4e00}'..='\u{9fff}'
        | '\u{f900}'..='\u{faff}'
        | '\u{20000}'..='\u{323af}' => 2,
        c if c.is_alphanumeric() || c == '_' => 1,
        c if c.is_whitespace() => 3,
        _ => 0,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn clicks_require_same_pane_position_and_interval() {
        let mut clicks = Clicks::default();
        let now = Instant::now();
        assert_eq!(clicks.next(&"%1", 1, 2, now), 1);
        assert_eq!(
            clicks.next(&"%1", 1, 2, now + Duration::from_millis(400)),
            2
        );
        assert_eq!(
            clicks.next(&"%1", 1, 2, now + Duration::from_millis(500)),
            3
        );
        assert_eq!(
            clicks.next(&"%1", 1, 2, now + Duration::from_millis(600)),
            1
        );
        assert_eq!(
            clicks.next(&"%2", 1, 2, now + Duration::from_millis(700)),
            1
        );
        assert_eq!(
            clicks.next(&"%2", 2, 2, now + Duration::from_millis(800)),
            1
        );
        assert_eq!(
            clicks.next(&"%2", 2, 2, now + Duration::from_millis(1201)),
            1
        );
        clicks.reset();
        assert_eq!(
            clicks.next(&"%2", 2, 2, now + Duration::from_millis(1202)),
            1
        );
    }
}
