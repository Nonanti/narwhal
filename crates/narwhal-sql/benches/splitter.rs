//! Statement splitter throughput benchmark.
//!
//! Four scenarios exercise the hot paths individually so a regression
//! in one (dollar-quoted strings, line comments, plain statements) can
//! be spotted in isolation.

use criterion::{black_box, criterion_group, criterion_main, Criterion, Throughput};
use narwhal_sql::splitter::{split_with, Dialect};

/// 200 plain CRUD statements separated by `;` — the common case.
fn plain_statements(count: usize) -> String {
    let mut s = String::with_capacity(count * 120);
    for i in 0..count {
        s.push_str(&format!(
            "SELECT id, name FROM users WHERE id = {i} AND status = 'active';\n"
        ));
    }
    s
}

/// 50 PL/pgSQL functions with dollar-quoted bodies. Exercises the
/// dollar-tag scanner (`memchr::memmem`) introduced in L3.
fn dollar_quoted(count: usize) -> String {
    let mut s = String::with_capacity(count * 320);
    for i in 0..count {
        s.push_str(&format!(
            "CREATE FUNCTION f_{i}() RETURNS int AS $body${{\n  \
             DECLARE x int := {i};\n  \
             BEGIN RETURN x * 2; END;\n}}$body$ LANGUAGE plpgsql;\n"
        ));
    }
    s
}

/// 500 statements with `-- line comment` interleaved. Stresses the
/// `LineComment` state.
fn with_line_comments(count: usize) -> String {
    let mut s = String::with_capacity(count * 60);
    for i in 0..count {
        s.push_str(&format!("-- statement {i}\nSELECT {i};\n"));
    }
    s
}

/// A single 100 KiB statement padded with whitespace — measures the
/// per-byte loop overhead on a degenerate input (one Statement output,
/// max scan).
fn one_huge_statement(bytes: usize) -> String {
    let mut s = String::with_capacity(bytes + 32);
    s.push_str("SELECT ");
    while s.len() < bytes {
        s.push_str("'x', ");
    }
    s.push_str("'y';");
    s
}

fn bench_splitter(c: &mut Criterion) {
    let mut group = c.benchmark_group("splitter");

    let plain = plain_statements(200);
    group.throughput(Throughput::Bytes(plain.len() as u64));
    group.bench_function("plain_200", |b| {
        b.iter(|| {
            let out = split_with(black_box(&plain), Dialect::Generic);
            black_box(out.len());
        });
    });

    let dollar = dollar_quoted(50);
    group.throughput(Throughput::Bytes(dollar.len() as u64));
    group.bench_function("dollar_quoted_50", |b| {
        b.iter(|| {
            let out = split_with(black_box(&dollar), Dialect::Postgres);
            black_box(out.len());
        });
    });

    let comments = with_line_comments(500);
    group.throughput(Throughput::Bytes(comments.len() as u64));
    group.bench_function("with_line_comments_500", |b| {
        b.iter(|| {
            let out = split_with(black_box(&comments), Dialect::Generic);
            black_box(out.len());
        });
    });

    let huge = one_huge_statement(100 * 1024);
    group.throughput(Throughput::Bytes(huge.len() as u64));
    group.bench_function("one_huge_statement_100kb", |b| {
        b.iter(|| {
            let out = split_with(black_box(&huge), Dialect::Generic);
            black_box(out.len());
        });
    });

    group.finish();
}

criterion_group!(benches, bench_splitter);
criterion_main!(benches);
