use criterion::{black_box, criterion_group, criterion_main, Criterion};
use wasmsh_ast::{Span, Word, WordPart};
use wasmsh_expand::{eval_arithmetic, expand_braces, expand_word};
use wasmsh_state::ShellState;

fn make_span() -> Span {
    Span { start: 0, end: 0 }
}

fn make_word(parts: Vec<WordPart>) -> Word {
    Word {
        parts,
        span: make_span(),
    }
}

fn bench_expand_simple(c: &mut Criterion) {
    let mut state = ShellState::new();
    state.set_var("FOO".into(), "hello".into());
    let word = make_word(vec![WordPart::Parameter("FOO".into())]);
    c.bench_function("expand_simple_var", |b| {
        b.iter(|| expand_word(black_box(&word), &mut state));
    });
}

fn bench_expand_literal(c: &mut Criterion) {
    let mut state = ShellState::new();
    let word = make_word(vec![WordPart::Literal("hello world literal text".into())]);
    c.bench_function("expand_literal", |b| {
        b.iter(|| expand_word(black_box(&word), &mut state));
    });
}

fn bench_brace_expansion(c: &mut Criterion) {
    c.bench_function("brace_range_100", |b| {
        b.iter(|| expand_braces(black_box("{1..100}")));
    });
    c.bench_function("brace_cartesian", |b| {
        b.iter(|| expand_braces(black_box("{a,b,c}{1,2,3}{x,y}")));
    });
}

fn bench_arithmetic(c: &mut Criterion) {
    let mut state = ShellState::new();
    c.bench_function("arith_simple", |b| {
        b.iter(|| eval_arithmetic(black_box("2 + 3 * 4"), &mut state));
    });
    c.bench_function("arith_complex", |b| {
        b.iter(|| eval_arithmetic(black_box("(1 + 2) * (3 + 4) - 5 % 3 + 10 / 2"), &mut state));
    });
}

criterion_group!(
    benches,
    bench_expand_simple,
    bench_expand_literal,
    bench_brace_expansion,
    bench_arithmetic
);
criterion_main!(benches);
