//! `:lint` command handler (v1.3 #9).
//!
//! Runs the [`narwhal_sql::lint`] rule set against the active editor
//! buffer and writes the findings to the status bar plus a fresh
//! editor tab when there are any. The buffer is read in full so the
//! cross-statement rules (cartesian, etc.) see the entire context.

use narwhal_sql::{lint, LintSeverity};

use super::AppCore;

impl AppCore {
    pub(super) async fn lint_buffer_command(&mut self) {
        let body = self.ui.tabs[self.ui.active_tab].editor.entire_text();
        if body.trim().is_empty() {
            self.ui.status.message = "lint: buffer is empty".into();
            return;
        }
        let findings = lint(&body);
        if findings.is_empty() {
            self.ui.status.message = "lint: clean".into();
            return;
        }

        // Build a human-readable report so we don't have to ship a
        // dedicated lint pane today. The report goes into a new tab
        // so the user can keep working with the original buffer
        // unchanged.
        let mut report = String::new();
        report.push_str(&format!("-- lint: {} finding(s)\n\n", findings.len()));
        for f in &findings {
            let sev = match f.severity {
                LintSeverity::Info => "info",
                LintSeverity::Warning => "warn",
                _ => "?",
            };
            report.push_str(&format!(
                "-- L{:>4} [{}] {}: {}\n",
                f.line, sev, f.rule, f.message
            ));
        }
        report.push('\n');
        report.push_str("-- original buffer below:\n");
        report.push_str(&body);

        self.new_tab().await;
        let tab = &mut self.ui.tabs[self.ui.active_tab];
        tab.editor.insert_str(&report);
        self.ui.status.message = format!("lint: {} finding(s) in new tab", findings.len());
    }
}
