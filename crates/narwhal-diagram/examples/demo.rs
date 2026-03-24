//! Quick visual check: cargo run -p narwhal-diagram --example demo
//!
//! Prints a Mermaid `erDiagram` and a Graphviz `dot` for a small
//! synthetic schema. Paste the Mermaid block into mermaid.live or pipe
//! the dot output into `dot -Tsvg -o schema.svg`.

use narwhal_core::schema::{
    Column, ForeignKey, Index, ReferentialAction, Table, TableKind, TableSchema, UniqueConstraint,
};
use narwhal_diagram::{DotRenderer, MermaidRenderer, Renderer, build};

fn main() {
    let tables = fixture();
    let model = build(&tables);

    println!("```mermaid");
    print!(
        "{}",
        MermaidRenderer::new()
            .with_title("narwhal demo schema")
            .render(&model)
    );
    println!("```");
    println!();
    println!("--- DOT ---");
    print!("{}", DotRenderer::new().render(&model));
}

fn fixture() -> Vec<TableSchema> {
    let users = TableSchema {
        table: Table {
            schema: "public".into(),
            name: "users".into(),
            kind: TableKind::Table,
        },
        columns: vec![
            col("id", "bigint", true, false),
            col("email", "varchar(255)", false, false),
            col("created_at", "timestamptz", false, false),
        ],
        indexes: vec![Index {
            name: "users_email_uq".into(),
            columns: vec!["email".into()],
            unique: true,
            primary: false,
        }],
        foreign_keys: Vec::new(),
        unique_constraints: Vec::new(),
    };

    let orders = TableSchema {
        table: Table {
            schema: "public".into(),
            name: "orders".into(),
            kind: TableKind::Table,
        },
        columns: vec![
            col("id", "bigint", true, false),
            col("user_id", "bigint", false, false),
            col("status", "text", false, true),
            col("total", "numeric(12,2)", false, false),
        ],
        indexes: Vec::new(),
        foreign_keys: vec![ForeignKey {
            name: "orders_user_fk".into(),
            columns: vec!["user_id".into()],
            referenced_schema: None,
            referenced_table: "users".into(),
            referenced_columns: vec!["id".into()],
            on_update: None,
            on_delete: Some(ReferentialAction::Cascade),
        }],
        unique_constraints: Vec::new(),
    };

    let order_items = TableSchema {
        table: Table {
            schema: "public".into(),
            name: "order_items".into(),
            kind: TableKind::Table,
        },
        columns: vec![
            col("order_id", "bigint", true, false),
            col("product_id", "bigint", true, false),
            col("qty", "int", false, false),
        ],
        indexes: Vec::new(),
        foreign_keys: vec![
            ForeignKey {
                name: "oi_order_fk".into(),
                columns: vec!["order_id".into()],
                referenced_schema: None,
                referenced_table: "orders".into(),
                referenced_columns: vec!["id".into()],
                on_update: None,
                on_delete: Some(ReferentialAction::Cascade),
            },
            ForeignKey {
                name: "oi_product_fk".into(),
                columns: vec!["product_id".into()],
                referenced_schema: None,
                referenced_table: "products".into(),
                referenced_columns: vec!["id".into()],
                on_update: None,
                on_delete: Some(ReferentialAction::Restrict),
            },
        ],
        unique_constraints: Vec::new(),
    };

    let products = TableSchema {
        table: Table {
            schema: "public".into(),
            name: "products".into(),
            kind: TableKind::Table,
        },
        columns: vec![
            col("id", "bigint", true, false),
            col("name", "text", false, false),
            col("price", "numeric(12,2)", false, false),
        ],
        indexes: Vec::new(),
        foreign_keys: Vec::new(),
        unique_constraints: Vec::new(),
    };

    let user_profile = TableSchema {
        table: Table {
            schema: "public".into(),
            name: "user_profile".into(),
            kind: TableKind::Table,
        },
        columns: vec![
            col("id", "bigint", true, false),
            col("user_id", "bigint", false, false),
            col("bio", "text", false, true),
        ],
        indexes: Vec::new(),
        foreign_keys: vec![ForeignKey {
            name: "user_profile_user_fk".into(),
            columns: vec!["user_id".into()],
            referenced_schema: None,
            referenced_table: "users".into(),
            referenced_columns: vec!["id".into()],
            on_update: None,
            on_delete: Some(ReferentialAction::Cascade),
        }],
        unique_constraints: vec![UniqueConstraint {
            name: "user_profile_user_uq".into(),
            columns: vec!["user_id".into()],
        }],
    };

    vec![users, orders, order_items, products, user_profile]
}

fn col(name: &str, ty: &str, pk: bool, nullable: bool) -> Column {
    Column {
        name: name.into(),
        data_type: ty.into(),
        nullable,
        primary_key: pk,
        default: None,
    }
}
