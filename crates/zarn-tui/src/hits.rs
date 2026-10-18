use ratatui::layout::Rect;
use std::cell::RefCell;
use std::sync::Mutex;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Hit {
    Popup(usize),
    Picker(usize),
    Permission(usize),
    Plan(usize),
    Confirm(usize),
    Tab(usize),
    Row(usize),
}

#[derive(Debug, Default)]
pub struct Hits(Mutex<Vec<(Rect, Hit)>>);

impl Hits {
    pub fn replace(&self, regions: Vec<(Rect, Hit)>) {
        *self.0.lock().unwrap_or_else(|e| e.into_inner()) = regions;
    }

    pub fn at(&self, column: u16, row: u16) -> Option<Hit> {
        self.0
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .iter()
            .rev()
            .find(|(rect, _)| {
                column >= rect.x
                    && column < rect.x.saturating_add(rect.width)
                    && row >= rect.y
                    && row < rect.y.saturating_add(rect.height)
            })
            .map(|(_, hit)| *hit)
    }

    pub fn all(&self) -> Vec<(Rect, Hit)> {
        self.0.lock().unwrap_or_else(|e| e.into_inner()).clone()
    }
}

thread_local! {
    static DRAWING: RefCell<Vec<(Rect, Hit)>> = const { RefCell::new(Vec::new()) };
}

pub fn begin() {
    DRAWING.with(|d| d.borrow_mut().clear());
}

pub fn add(rect: Rect, hit: Hit) {
    if rect.width == 0 || rect.height == 0 {
        return;
    }
    DRAWING.with(|d| d.borrow_mut().push((rect, hit)));
}

pub fn row(area: Rect, y: u16, hit: Hit) {
    if y < area.y || y >= area.y.saturating_add(area.height) {
        return;
    }
    add(
        Rect {
            x: area.x,
            y,
            width: area.width,
            height: 1,
        },
        hit,
    );
}

pub fn finish() -> Vec<(Rect, Hit)> {
    DRAWING.with(|d| std::mem::take(&mut *d.borrow_mut()))
}
