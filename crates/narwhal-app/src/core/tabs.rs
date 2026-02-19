//! Tab/result-tab management extracted from `core.rs` (L21).
//!
//! Owns the small `:tabnew`/`:tabclose`/`gt`/`gT` mutators and the
//! status-bar formatter that lists tab names. The internal data lives
//! on [`super::AppCore`] (see `tabs: Vec<Tab>` + `active_tab`).
use narwhal_tui::Pane;

use super::{AppCore, Tab};

impl AppCore {
    pub(super) fn editor_title_with_tabs(&self) -> String {
        let driver = self.session.as_ref().map(|s| s.driver.display_name());
        let base = match driver {
            Some(d) => format!("editor · {d}"),
            None => "editor".to_owned(),
        };
        if self.tabs.len() == 1 {
            return base;
        }
        let labels: Vec<String> = self
            .tabs
            .iter()
            .enumerate()
            .map(|(i, t)| {
                if i == self.active_tab {
                    format!("[{}*] {}", i + 1, t.name)
                } else {
                    format!("[{}] {}", i + 1, t.name)
                }
            })
            .collect();
        format!("{base} · {}", labels.join("  "))
    }

    pub(super) fn new_tab(&mut self) {
        if self.process.running {
            self.status.message = "cannot open a new tab while a query is running".into();
            return;
        }
        let id = self.next_tab_id as u64;
        let name = format!("untitled-{id}");
        self.next_tab_id += 1;
        self.tabs.push(Tab::new(id, name));
        self.active_tab = self.tabs.len() - 1;
        self.status.message = format!("tab {} opened", self.active_tab + 1);
        self.focus = Pane::Editor;
    }

    pub(super) fn close_tab(&mut self) {
        if self.process.running {
            self.status.message = "cannot close a tab while a query is running".into();
            return;
        }
        if self.tabs.len() == 1 {
            self.status.message = "last tab; use :q to quit".into();
            return;
        }
        self.tabs.remove(self.active_tab);
        if self.active_tab >= self.tabs.len() {
            self.active_tab = self.tabs.len() - 1;
        }
        self.status.message = format!("tab closed; now on {}", self.active_tab + 1);
    }

    pub(super) fn cycle_tab(&mut self, delta: i32) {
        if self.process.running {
            self.status.message = "cannot switch tabs while a query is running".into();
            return;
        }
        if self.tabs.len() <= 1 {
            return;
        }
        let len = self.tabs.len() as i32;
        let next = ((self.active_tab as i32) + delta).rem_euclid(len) as usize;
        self.active_tab = next;
        self.status.message = format!(
            "tab {} of {} · {}",
            self.active_tab + 1,
            self.tabs.len(),
            self.tabs[self.active_tab].name
        );
    }

    /// Cycle through the per-statement results inside the active tab's
    /// [`super::ResultBundle`]. `delta` +1 goes forward, −1 goes backward.
    /// Does nothing when the bundle has only one result.
    pub(super) fn cycle_result_tab(&mut self, delta: i32) {
        let bundle = &mut self.tabs[self.active_tab].results;
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
        self.status.message = format!("result {} of {total}", active + 1);
    }
}
