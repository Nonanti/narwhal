//! Sidebar item types surfaced by the schema browser.

use narwhal_core::TableKind;
use uuid::Uuid;

#[derive(Debug, Clone)]
pub enum SidebarItem {
    Connection {
        #[allow(dead_code)]
        id: Uuid,
        name: String,
        driver: String,
        active: bool,
    },
    Schema {
        name: String,
    },
    Table {
        schema: String,
        name: String,
        kind: TableKind,
    },
}
