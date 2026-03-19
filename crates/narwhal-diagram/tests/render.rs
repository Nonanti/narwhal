//! Snapshot-style assertions for the Mermaid and DOT renderers.
//!
//! We avoid `insta` snapshot files here so the suite stays self-contained;
//! the expected strings are short enough to embed inline.

use narwhal_core::schema::{Column, ForeignKey, Table, TableKind, TableSchema};
use narwhal_diagram::{
    build, build_with_logical, Cardinality, DotRenderer, LogicalRelation, MermaidRenderer,
    QualifiedName, Renderer,
};

fn fixture() -> Vec<TableSchema> {
    let users = TableSchema {
        table: Table {
            schema: "public".into(),
            name: "users".into(),
            kind: TableKind::Table,
        },
        columns: vec![
            Column {
                name: "id".into(),
                data_type: "bigint".into(),
                nullable: false,
                primary_key: true,
                default: None,
            },
            Column {
                name: "email".into(),
                data_type: "varchar(255)".into(),
                nullable: false,
                primary_key: false,
                default: None,
            },
        ],
        indexes: Vec::new(),
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
            Column {
                name: "id".into(),
                data_type: "bigint".into(),
                nullable: false,
                primary_key: true,
                default: None,
            },
            Column {
                name: "user_id".into(),
                data_type: "bigint".into(),
                nullable: false,
                primary_key: false,
                default: None,
            },
        ],
        indexes: Vec::new(),
        foreign_keys: vec![ForeignKey {
            name: "orders_user_fk".into(),
            columns: vec!["user_id".into()],
            referenced_schema: None,
            referenced_table: "users".into(),
            referenced_columns: vec!["id".into()],
            on_update: None,
            on_delete: None,
        }],
        unique_constraints: Vec::new(),
    };

    vec![users, orders]
}

#[test]
fn mermaid_basic_shape() {
    let model = build(&fixture());
    let out = MermaidRenderer::new().render(&model);

    assert!(out.starts_with("erDiagram\n"), "must start with erDiagram");
    // Schema-qualified id becomes `public_users`.
    assert!(out.contains("public_users {"));
    assert!(out.contains("public_orders {"));
    // varchar(255) collapses to `varchar_255`.
    assert!(out.contains("varchar_255 email"));
    // Parent (users) on the left, child (orders) on the right.
    assert!(out.contains("public_users ||--o{ public_orders : \"user_id\""));
}

#[test]
fn mermaid_title_front_matter() {
    let model = build(&fixture());
    let out = MermaidRenderer::new().with_title("Public schema").render(&model);
    assert!(out.starts_with("---\ntitle: Public schema\n---\nerDiagram\n"));
}

#[test]
fn mermaid_without_columns_emits_empty_blocks() {
    let model = build(&fixture());
    let out = MermaidRenderer::new().without_columns().render(&model);
    assert!(out.contains("public_users { }"));
    assert!(!out.contains("varchar"));
}

#[test]
fn dot_basic_shape() {
    let model = build(&fixture());
    let out = DotRenderer::new().render(&model);

    assert!(out.starts_with("digraph schema {\n"));
    assert!(out.contains("rankdir=LR;"));
    // HTML record label header.
    assert!(out.contains("<b>public.users</b>"));
    // PK marker.
    assert!(out.contains("[PK] id : bigint"));
    // FK marker.
    assert!(out.contains("[FK] user_id : bigint"));
    // Edge with arrowhead=crow for many side.
    assert!(out.contains("public_orders:user_id -> public_users:id"));
    assert!(out.contains("arrowhead=crow"));
    assert!(out.trim_end().ends_with("}"));
}

#[test]
fn dot_with_rankdir_tb() {
    let model = build(&fixture());
    let out = DotRenderer::new().with_rankdir("TB").render(&model);
    assert!(out.contains("rankdir=TB;"));
}

#[test]
fn mermaid_logical_edge_uses_dashed_notation() {
    let model_in = fixture();
    let logical = vec![LogicalRelation {
        from: QualifiedName::new("public", "orders"),
        to: QualifiedName::new("public", "users"),
        columns: vec![("user_id".into(), "id".into())],
        cardinality: Cardinality::ManyToOne,
        note: Some("shadow".into()),
    }];
    let (model, _) = build_with_logical(&model_in, &logical);
    let out = MermaidRenderer::new().render(&model);

    // FK edge: solid `--`.
    assert!(
        out.contains("public_users ||--o{ public_orders : \"user_id\""),
        "FK line missing:\n{out}"
    );
    // Logical edge: dashed `..` and the [logical] label suffix.
    assert!(
        out.contains("public_users }o..|| public_orders : \"user_id [logical]\""),
        "logical line missing:\n{out}"
    );
}

#[test]
fn dot_logical_edge_is_dashed_and_grey() {
    let model_in = fixture();
    let logical = vec![LogicalRelation {
        from: QualifiedName::new("public", "orders"),
        to: QualifiedName::new("public", "users"),
        columns: vec![("user_id".into(), "id".into())],
        cardinality: Cardinality::ManyToOne,
        note: None,
    }];
    let (model, _) = build_with_logical(&model_in, &logical);
    let out = DotRenderer::new().render(&model);
    assert!(
        out.contains("style=dashed"),
        "logical edge must be dashed in DOT:\n{out}"
    );
    assert!(out.contains("[logical]"));
}

#[test]
fn dot_unknown_rankdir_falls_back_to_lr() {
    let model = build(&fixture());
    let out = DotRenderer::new().with_rankdir("XX").render(&model);
    assert!(out.contains("rankdir=LR;"));
}
