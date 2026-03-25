//! T2-T4-D: `:pivot` command dispatch.
//!
//! Activating the pivot pane only sets a [`PivotConfig`] on the active
//! tab; the actual cell grid is derived inside the render path
//! (`render_helpers::pivot_payload`) so the pane "streams" automatically
//! as `RunUpdate::RowsAppended` mutates the underlying `Vec<Row>`.

use narwhal_pivot::{AggKind, PivotConfig};

use crate::commands::{PivotAggArg, PivotArg};

use super::AppCore;

impl AppCore {
    pub(super) async fn pivot_command(&mut self, arg: PivotArg) {
        let tab = &mut self.ui.tabs[self.ui.active_tab];
        match arg {
            PivotArg::Off => {
                if tab.pivot.take().is_some() {
                    self.ui.status.message = "pivot: off".into();
                } else {
                    self.ui.status.message = "pivot: already hidden".into();
                }
            }
            PivotArg::On {
                rows,
                cols,
                value,
                agg,
            } => {
                let agg_kind = match agg {
                    PivotAggArg::Count => AggKind::Count,
                    PivotAggArg::Sum => AggKind::Sum,
                    PivotAggArg::Avg => AggKind::Avg,
                    PivotAggArg::Min => AggKind::Min,
                    PivotAggArg::Max => AggKind::Max,
                };
                let mut config = PivotConfig::new(agg_kind);
                config.row_dims = rows;
                config.col_dim = cols;
                config.value = value;
                let label = agg_kind.label();
                tab.pivot = Some(config);
                self.ui.status.message = format!("pivot: on · agg={label}");
            }
        }
    }
}
