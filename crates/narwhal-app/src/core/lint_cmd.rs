//! `:lint` command handler (v1.3 #9).
//!
//! Runs the [`narwhal_sql::lint_with_dialect`] rule set against the
//! active editor buffer and writes the findings to the status bar
//! plus a fresh editor tab when there are any. The buffer is read
//! in full so the cross-statement rules (cartesian, etc.) see the
//! entire context.

use narwhal_commands::template;
use narwhal_sql::{LintSeverity, lint_with_dialect, splitter::Dialect};

use super::AppCore;

impl AppCore {
    /// v1.3 #10: `:tpl <name>` insertion.
    pub(super) async fn insert_template_command(&mut self, name: Option<String>) {
        let Some(name) = name else {
            self.ui.status.message =
                format!("tpl: known templates: {}", template::list().join(", "));
            return;
        };
        match template::lookup(&name) {
            Some(body) => {
                let tab = &mut self.ui.tabs[self.ui.active_tab];
                tab.editor.insert_str(body);
                self.ui.status.message = format!("tpl: inserted '{name}'");
            }
            None => {
                self.ui.status.message = format!(
                    "tpl: unknown '{name}'. Try one of: {}",
                    template::list().join(", ")
                );
            }
        }
    }

    pub(super) async fn lint_buffer_command(&mut self) {
        let body = self.ui.tabs[self.ui.active_tab].editor.entire_text();
        if body.trim().is_empty() {
            self.ui.status.message = "lint: buffer is empty".into();
            return;
        }
        // M-4: use the active session's dialect so PG dollar-quoted
        // strings and MySQL backtick identifiers don't fragment the
        // splitter. When no session is open, fall back to Generic.
        let dialect = self
            .session
            .active
            .as_ref()
            .map_or(Dialect::Generic, |s| s.dialect());
        let findings = lint_with_dialect(&body, dialect);
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
