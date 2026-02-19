//! Tab/result-tab management extracted from `core.rs` (L21).
//!
//! Owns the small `:tabnew`/`:tabclose`/`gt`/`gT` mutators and the
//! status-bar formatter that lists tab names. The internal data lives
//! on [`super::AppCore`] (see `tabs: Vec<Tab>` + `active_tab`).
use narwhal_tui::Pane;

use super::{AppCore, Tab};

impl AppCore {
    pub(super) fn editor_title_with_tabs(&self) -> String {
        let driver = self
            .session
            .active
            .as_ref()
            .map(|s| s.driver.display_name());
        let base = match driver {
            Some(d) => format!("editor · {d}"),
            None => "editor".to_owned(),
        };
        if self.ui.tabs.len() == 1 {
            return base;
        }
        let labels: Vec<String> = self
            .ui
            .tabs
            .iter()
            .enumerate()
            .map(|(i, t)| {
                if i == self.ui.active_tab {
                    format!("[{}*] {}", i + 1, t.name)
                } else {
                    format!("[{}] {}", i + 1, t.name)
                }
            })
            .collect();
        format!("{base} · {}", labels.join("  "))
    }

    pub(super) async fn new_tab(&mut self) {
        if self.process.running {
            self.ui.status.message = "cannot open a new tab while a query is running".into();
            return;
        }
        let id = self.ui.next_tab_id as u64;
        let name = format!("untitled-{id}");
        self.ui.next_tab_id += 1;
        self.ui.tabs.push(Tab::new(id, name));
        self.ui.active_tab = self.ui.tabs.len() - 1;
        self.ui.status.message = format!("tab {} opened", self.ui.active_tab + 1);
        self.ui.focus = Pane::Editor;
    }

    pub(super) async fn close_tab(&mut self) {
        if self.process.running {
            self.ui.status.message = "cannot close a tab while a query is running".into();
            return;
        }
        if self.ui.tabs.len() == 1 {
            self.ui.status.message = "last tab; use :q to quit".into();
            return;
        }
        self.ui.tabs.remove(self.ui.active_tab);
        if self.ui.active_tab >= self.ui.tabs.len() {
            self.ui.active_tab = self.ui.tabs.len() - 1;
        }
        self.ui.status.message = format!("tab closed; now on {}", self.ui.active_tab + 1);
    }

    pub(super) async fn cycle_tab(&mut self, delta: i32) {
        if self.process.running {
            self.ui.status.message = "cannot switch tabs while a query is running".into();
            return;
        }
        if self.ui.tabs.len() <= 1 {
            return;
        }
        let len = self.ui.tabs.len() as i32;
        let next = ((self.ui.active_tab as i32) + delta).rem_euclid(len) as usize;
        self.ui.active_tab = next;
        self.ui.status.message = format!(
            "tab {} of {} · {}",
            self.ui.active_tab + 1,
            self.ui.tabs.len(),
            self.ui.tabs[self.ui.active_tab].name
        );
    }

    /// Cycle through the per-statement results inside the active tab's
    /// [`super::ResultBundle`]. `delta` +1 goes forward, −1 goes backward.
    /// Does nothing when the bundle has only one result.
    pub(super) async fn cycle_result_tab(&mut self, delta: i32) {
        let bundle = &mut self.ui.tabs[self.ui.active_tab].results;
        if !bundle.is_multi() {
            return;
        }
        match delta {
            1 => bundle.next(),
            -1 => bundle.prev(),
            _ => {}
        }
        let active = bundle.active;
        let total = bundle.states.len();
        self.ui.status.message = format!("result {} of {total}", active + 1);
    }
}
