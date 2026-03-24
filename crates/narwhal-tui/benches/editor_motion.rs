//! Editor word-motion benchmark.
//!
//! `move_word_forward` and its sibling are hit on every `w` / `b`
//! keystroke. Today they call `entire_text()` which joins every line
//! into a fresh `String` on every invocation — a regression magnet
//! once buffers grow.

use criterion::{BenchmarkId, Criterion, black_box, criterion_group, criterion_main};
use narwhal_domain::Motion;
use narwhal_tui::EditorBuffer;

fn lorem_buffer(lines: usize) -> EditorBuffer {
    let mut buf = EditorBuffer::new();
    let snippet = "SELECT id, name, value FROM widgets WHERE active = true AND score > 12345;";
    let mut body = String::with_capacity(lines * (snippet.len() + 1));
    for i in 0..lines {
        body.push_str(snippet);
        if i + 1 != lines {
            body.push('\n');
        }
    }
    buf.insert_str(&body);
    buf.set_cursor(0, 0);
    buf
}

fn bench_motion(c: &mut Criterion) {
    let mut group = c.benchmark_group("editor_motion");
    for &lines in &[50usize, 500, 5_000] {
        let template = lorem_buffer(lines);
        let id = BenchmarkId::new("word_forward_x10", lines);
        group.bench_with_input(id, &template, |b, template| {
            b.iter_batched(
                || {
                    // Cheap clone: lines is a Vec<String>.
                    let mut clone = EditorBuffer::new();
                    clone.insert_str(&template.entire_text());
                    clone.set_cursor(0, 0);
                    clone
                },
                |mut buf| {
                    // Ten `w` presses — covers the typical jump-around
                    // burst a user issues while skimming a query.
                    buf.apply_motion(Motion::WordForward, 10);
                    black_box(buf.cursor());
                },
                criterion::BatchSize::SmallInput,
            );
        });
    }
    group.finish();
}

criterion_group!(benches, bench_motion);
criterion_main!(benches);
