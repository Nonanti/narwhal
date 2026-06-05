//! Result-pane sort-comparator benchmark.
//!
//! Three scenarios isolate the cost of comparing different value
//! variants. The Json bench is the headline number — today every
//! comparison materialises both sides via `.to_string()`.

use criterion::{Criterion, black_box, criterion_group, criterion_main};
use narwhal_core::Value;
use narwhal_tui::compare_values;

fn make_int_rows(n: usize) -> Vec<Value> {
    (0..n)
        .map(|i| Value::Int(((i * 1_103_515_245 + 12_345) % 1_000_000) as i64))
        .collect()
}

fn make_string_rows(n: usize) -> Vec<Value> {
    (0..n)
        .map(|i| {
            Value::String(format!(
                "row_{:08}",
                (i * 2_654_435_761_usize) & 0xFFFF_FFFF
            ))
        })
        .collect()
}

fn make_json_rows(n: usize) -> Vec<Value> {
    (0..n)
        .map(|i| {
            Value::Json(serde_json::json!({
                "id": i,
                "name": format!("u{i}"),
                "tags": ["alpha", "beta", "gamma"],
                "active": i % 2 == 0,
            }))
        })
        .collect()
}

fn bench_sort(c: &mut Criterion) {
    let mut group = c.benchmark_group("sort");

    let ints = make_int_rows(10_000);
    group.bench_function("int_10k", |b| {
        b.iter_batched(
            || ints.clone(),
            |mut v| {
                v.sort_by(|a, b| compare_values(Some(a), Some(b)));
                black_box(v.len());
            },
            criterion::BatchSize::SmallInput,
        );
    });

    let strings = make_string_rows(10_000);
    group.bench_function("string_10k", |b| {
        b.iter_batched(
            || strings.clone(),
            |mut v| {
                v.sort_by(|a, b| compare_values(Some(a), Some(b)));
                black_box(v.len());
            },
            criterion::BatchSize::SmallInput,
        );
    });

    let jsons = make_json_rows(2_000);
    group.bench_function("json_2k", |b| {
        b.iter_batched(
            || jsons.clone(),
            |mut v| {
                v.sort_by(|a, b| compare_values(Some(a), Some(b)));
                black_box(v.len());
            },
            criterion::BatchSize::SmallInput,
        );
    });

    group.finish();
}

criterion_group!(benches, bench_sort);
criterion_main!(benches);
