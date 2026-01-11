//! Journal append-throughput benchmark.
//!
//! Each iteration appends one entry; criterion converts the per-iter
//! time to an effective throughput. The benchmark uses
//! `tempfile::tempdir` so the on-disk file is recreated per run and the
//! fsync cost is realistic (no page-cache cheating across runs).

use criterion::{black_box, criterion_group, criterion_main, BenchmarkId, Criterion};
use narwhal_history::{HistoryEntry, Journal};
use tokio::runtime::Runtime;

fn entry(sql: &str) -> HistoryEntry {
    HistoryEntry::success(sql)
}

fn bench_append(c: &mut Criterion) {
    let rt = Runtime::new().expect("tokio runtime");
    let mut group = c.benchmark_group("append");
    group.sample_size(20); // each sample writes + fsyncs, keep wall-clock sane

    for &payload_kb in &[0usize, 1, 8] {
        let sql = if payload_kb == 0 {
            "SELECT 1".to_owned()
        } else {
            format!("SELECT '{}' AS p", "x".repeat(payload_kb * 1024))
        };
        let id = BenchmarkId::new("payload_kb", payload_kb);
        group.bench_with_input(id, &sql, |b, sql| {
            // Fresh tempdir per timing run so the inode benefits from a
            // cold metadata cache once and then stays warm — closer to
            // the steady-state real-world write path.
            let dir = tempfile::tempdir().expect("tempdir");
            let path = dir.path().join("history.jsonl");
            let journal = rt.block_on(Journal::open(&path)).expect("open");
            let entry = entry(sql);
            b.to_async(&rt).iter(|| async {
                journal.append(black_box(&entry)).await.expect("append");
            });
            drop(journal);
            drop(dir);
        });
    }

    group.finish();
}

criterion_group!(benches, bench_append);
criterion_main!(benches);
