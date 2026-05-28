//! `:chart` command dispatch.
//!
//! Activating the chart pane only sets a [`ChartConfig`] on the active
//! tab; deriving the actual datapoints happens inside the render path
//! (`render_helpers::chart_payload`) so the chart "streams" automatically
//! as `RunUpdate::RowsAppended` mutates the underlying `Vec<Row>`.

use crate::commands::{ChartArg, ChartKindArg};

use super::AppCore;
use super::chart::{ChartConfig, ChartKind};

impl AppCore {
    pub(super) async fn chart_command(&mut self, arg: ChartArg) {
        let tab = &mut self.ui.tabs[self.ui.active_tab];
        match arg {
            ChartArg::Off => {
                if tab.chart.take().is_some() {
                    self.ui.status.message = "chart: off".into();
                } else {
                    self.ui.status.message = "chart: already hidden".into();
                }
            }
            ChartArg::On {
                kind,
                title,
                x_col,
                y_col,
            } => {
                let kind = match kind {
                    ChartKindArg::Bar => ChartKind::Bar,
                    ChartKindArg::Line => ChartKind::Line,
                    ChartKindArg::Sparkline => ChartKind::Sparkline,
                };
                let mut config = ChartConfig::new(kind);
                config.title = title;
                config.x_col = x_col;
                config.y_col = y_col;
                let kind_label = kind.label();
                tab.chart = Some(config);
                self.ui.status.message = format!("chart: {kind_label} on");
            }
        }
    }
}
