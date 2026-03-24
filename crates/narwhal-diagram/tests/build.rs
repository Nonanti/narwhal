//! Integration tests for the `build` / `focused` / `impact` pipeline.

use narwhal_core::schema::{
    Column, ForeignKey, Index, ReferentialAction, Table, TableKind, TableSchema, UniqueConstraint,
};
use narwhal_diagram::{
    BuildDiagnostic, Cardinality, EdgeKind, LogicalRelation, QualifiedName, build,
    build_with_logical, focused, impact,
};

fn table(name: &str, columns: Vec<Column>) -> TableSchema {
    TableSchema {
        table: Table {
            schema: "public".into(),
            name: name.into(),
            kind: TableKind::Table,
        },
        columns,
        indexes: Vec::new(),
        foreign_keys: Vec::new(),
        unique_constraints: Vec::new(),
    }
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

fn fk(name: &str, cols: &[&str], to_table: &str, to_cols: &[&str]) -> ForeignKey {
    ForeignKey {
        name: name.into(),
        columns: cols.iter().map(|s| (*s).into()).collect(),
        referenced_schema: None,
        referenced_table: to_table.into(),
        referenced_columns: to_cols.iter().map(|s| (*s).into()).collect(),
        on_update: None,
        on_delete: Some(ReferentialAction::Cascade),
    }
}

fn fixture() -> Vec<TableSchema> {
    let mut users = table(
        "users",
        vec![
            col("id", "bigint", true, false),
            col("email", "text", false, false),
        ],
    );
    users.indexes.push(Index {
        name: "users_email_uq".into(),
        columns: vec!["email".into()],
        unique: true,
        primary: false,
    });

    let mut orders = table(
        "orders",
        vec![
            col("id", "bigint", true, false),
            col("user_id", "bigint", false, false),
            col("status", "text", false, true),
        ],
    );
    orders
        .foreign_keys
        .push(fk("orders_user_fk", &["user_id"], "users", &["id"]));

    let mut order_items = table(
        "order_items",
        vec![
            col("order_id", "bigint", true, false),
            col("product_id", "bigint", true, false),
            col("qty", "int", false, false),
        ],
    );
    order_items
        .foreign_keys
        .push(fk("oi_order_fk", &["order_id"], "orders", &["id"]));

    // Optional 1-to-1: user_profile.user_id FK + UNIQUE → ||--||.
    let mut profile = table(
        "user_profile",
        vec![
            col("id", "bigint", true, false),
            col("user_id", "bigint", false, false),
            col("bio", "text", false, true),
        ],
    );
    profile
        .foreign_keys
        .push(fk("user_profile_user_fk", &["user_id"], "users", &["id"]));
    profile.unique_constraints.push(UniqueConstraint {
        name: "user_profile_user_uq".into(),
        columns: vec!["user_id".into()],
    });

    // Nullable FK example: audit row may not belong to any user.
    let mut audit = table(
        "audit",
        vec![
            col("id", "bigint", true, false),
            col("actor_id", "bigint", false, true),
        ],
    );
    audit
        .foreign_keys
        .push(fk("audit_actor_fk", &["actor_id"], "users", &["id"]));

    vec![users, orders, order_items, profile, audit]
}

fn qn(name: &str) -> QualifiedName {
    QualifiedName::new("public", name)
}

#[test]
fn nodes_and_edges_resolved() {
    let model = build(&fixture());
    assert_eq!(model.nodes.len(), 5);
    assert_eq!(model.edges.len(), 4);
}

#[test]
fn cardinalities_are_computed_from_constraints() {
    let model = build(&fixture());

    let card = |from: &str, to: &str| {
        model
            .edges
            .iter()
            .find(|e| e.from == qn(from) && e.to == qn(to))
            .map(|e| e.cardinality)
    };

    // NOT NULL FK, not unique → 1-to-many.
    assert_eq!(card("orders", "users"), Some(Cardinality::OneToMany));
    // Composite PK on (order_id, product_id) means order_id is part of PK
    // but the FK is only `order_id` so it is not unique on its own.
    assert_eq!(card("order_items", "orders"), Some(Cardinality::OneToMany));
    // FK with UNIQUE constraint covering exactly the FK column → 1-to-1.
    assert_eq!(card("user_profile", "users"), Some(Cardinality::OneToOne));
    // Nullable FK → 0..1-to-many.
    assert_eq!(card("audit", "users"), Some(Cardinality::ZeroOrOneToMany));
}

#[test]
fn fk_columns_are_flagged_on_nodes() {
    let model = build(&fixture());
    let orders = model.node(&qn("orders")).expect("orders node");
    let user_id = orders.columns.iter().find(|c| c.name == "user_id").unwrap();
    assert!(user_id.foreign_key);
    assert!(!user_id.primary_key);
}

#[test]
fn focused_keeps_one_hop_neighbours_in_both_directions() {
    let model = build(&fixture());
    let view = focused(&model, &qn("orders"), 1);

    let names: Vec<_> = view
        .nodes
        .iter()
        .map(|n| n.qualified.name.clone())
        .collect();
    assert!(names.contains(&"orders".into()));
    assert!(names.contains(&"users".into()), "out-edge target");
    assert!(names.contains(&"order_items".into()), "in-edge source");
    assert!(
        !names.contains(&"audit".into()),
        "2 hops away, must be excluded"
    );
}

#[test]
fn focused_unknown_table_returns_empty() {
    let model = build(&fixture());
    let view = focused(&model, &qn("does_not_exist"), 2);
    assert!(view.is_empty());
}

#[test]
fn impact_tree_walks_reverse_fk_recursively() {
    let model = build(&fixture());
    let tree = impact(&model, &qn("users"));
    assert_eq!(tree.root, qn("users"));

    let names: Vec<_> = tree.inbound.iter().map(|n| n.table.name.clone()).collect();
    assert!(names.contains(&"orders".into()));
    assert!(names.contains(&"user_profile".into()));
    assert!(names.contains(&"audit".into()));

    let orders_node = tree
        .inbound
        .iter()
        .find(|n| n.table.name == "orders")
        .unwrap();
    let child_names: Vec<_> = orders_node
        .children
        .iter()
        .map(|n| n.table.name.clone())
        .collect();
    assert_eq!(child_names, vec!["order_items".to_string()]);
}

#[test]
fn impact_tree_does_not_loop_on_cycles() {
    // self-referential FK
    let mut t = table(
        "node",
        vec![
            col("id", "bigint", true, false),
            col("parent_id", "bigint", false, true),
        ],
    );
    t.foreign_keys
        .push(fk("node_parent_fk", &["parent_id"], "node", &["id"]));

    let model = build(&[t]);
    let tree = impact(&model, &qn("node"));
    // The self-edge is found once, then visited blocks recursion.
    assert_eq!(tree.inbound.len(), 1);
    assert!(tree.inbound[0].children.is_empty());
}

#[test]
fn logical_relation_adds_dashed_edge() {
    let model_in = fixture();
    let logical = vec![LogicalRelation {
        from: QualifiedName::new("public", "audit"),
        to: QualifiedName::new("public", "orders"),
        columns: vec![("actor_id".into(), "id".into())],
        cardinality: Cardinality::ManyToOne,
        note: Some("cross-shard pruning".into()),
    }];
    let (model, diags) = build_with_logical(&model_in, &logical);
    assert!(diags.is_empty(), "clean inputs produce no diagnostics");

    let edge = model
        .edges
        .iter()
        .find(|e| matches!(e.kind, EdgeKind::Logical { .. }))
        .expect("logical edge present");
    assert_eq!(edge.cardinality, Cardinality::ManyToOne);
    assert!(edge.label().contains("[logical]"));
    if let EdgeKind::Logical { note } = &edge.kind {
        assert_eq!(note.as_deref(), Some("cross-shard pruning"));
    }
}

#[test]
fn logical_relation_with_unknown_table_is_dropped_with_diagnostic() {
    let model_in = fixture();
    let logical = vec![LogicalRelation {
        from: QualifiedName::new("public", "events"), // not in fixture
        to: QualifiedName::new("public", "users"),
        columns: vec![("user_id".into(), "id".into())],
        cardinality: Cardinality::ManyToOne,
        note: None,
    }];
    let (model, diags) = build_with_logical(&model_in, &logical);
    assert!(
        !model.edges.iter().any(|e| e.kind.is_logical()),
        "unknown-from edge must not be wired"
    );
    assert_eq!(diags.len(), 1);
    assert!(matches!(
        diags[0],
        BuildDiagnostic::UnknownTable { side: "from", .. }
    ));
}

#[test]
fn logical_relation_with_unknown_column_is_dropped_with_diagnostic() {
    let model_in = fixture();
    let logical = vec![LogicalRelation {
        from: QualifiedName::new("public", "audit"),
        to: QualifiedName::new("public", "users"),
        columns: vec![("nope_id".into(), "id".into())],
        cardinality: Cardinality::ManyToOne,
        note: None,
    }];
    let (model, diags) = build_with_logical(&model_in, &logical);
    assert!(!model.edges.iter().any(|e| e.kind.is_logical()));
    assert_eq!(diags.len(), 1);
    assert!(matches!(diags[0], BuildDiagnostic::UnknownColumn { .. }));
}

#[test]
fn cardinality_parse_accepts_kebab_and_aliases() {
    assert_eq!(
        Cardinality::parse("one-to-many"),
        Some(Cardinality::OneToMany)
    );
    assert_eq!(
        Cardinality::parse("1-to-many"),
        Some(Cardinality::OneToMany)
    );
    assert_eq!(
        Cardinality::parse("many-to-one"),
        Some(Cardinality::ManyToOne)
    );
    assert_eq!(
        Cardinality::parse(" MANY_TO_MANY "),
        Some(Cardinality::ManyToMany)
    );
    assert_eq!(Cardinality::parse("bogus"), None);
}

#[test]
fn logical_edge_participates_in_focused_and_impact() {
    let model_in = fixture();
    let logical = vec![LogicalRelation {
        from: QualifiedName::new("public", "audit"),
        to: QualifiedName::new("public", "orders"),
        columns: vec![("actor_id".into(), "id".into())],
        cardinality: Cardinality::ManyToOne,
        note: None,
    }];
    let (model, _) = build_with_logical(&model_in, &logical);

    // Focused: orders — 1-hop must include audit through the logical edge.
    let view = focused(&model, &qn("orders"), 1);
    let names: Vec<_> = view
        .nodes
        .iter()
        .map(|n| n.qualified.name.clone())
        .collect();
    assert!(
        names.contains(&"audit".into()),
        "logical edge should connect audit to orders"
    );

    // Impact: orders — audit appears as a reverse-FK inbound through logical edge.
    let tree = impact(&model, &qn("orders"));
    let inbound: Vec<_> = tree.inbound.iter().map(|n| n.table.name.clone()).collect();
    assert!(inbound.contains(&"audit".into()));
}

#[test]
fn cross_schema_fks_are_dropped() {
    let mut users = table("users", vec![col("id", "bigint", true, false)]);
    let mut orders = table(
        "orders",
        vec![
            col("id", "bigint", true, false),
            col("user_id", "bigint", false, false),
        ],
    );
    // FK pointing into a schema that is not in the model.
    let mut cross = fk("x", &["user_id"], "users", &["id"]);
    cross.referenced_schema = Some("other".into());
    orders.foreign_keys.push(cross);
    users.table.schema = "public".into();
    orders.table.schema = "public".into();

    let model = build(&[users, orders]);
    assert!(model.edges.is_empty(), "cross-schema edge must be dropped");
}
