use criterion::{black_box, criterion_group, criterion_main, BenchmarkId, Criterion};

fn bench_parse_simple(c: &mut Criterion) {
    c.bench_function("parse_simple_echo", |b| {
        b.iter(|| wasmsh_parse::parse(black_box("echo hello world")));
    });
}

fn bench_parse_pipeline(c: &mut Criterion) {
    c.bench_function("parse_pipeline", |b| {
        b.iter(|| {
            wasmsh_parse::parse(black_box(
                "cat file.txt | grep pattern | sort | uniq -c | head -10",
            ))
        });
    });
}

fn bench_parse_complex_script(c: &mut Criterion) {
    let script = r#"
        for i in 1 2 3 4 5; do
            if [ "$i" -gt 3 ]; then
                echo "big: $i"
            else
                echo "small: $i"
            fi
        done
    "#;
    c.bench_function("parse_complex_script", |b| {
        b.iter(|| wasmsh_parse::parse(black_box(script)));
    });
}

fn bench_parse_scaling(c: &mut Criterion) {
    let mut group = c.benchmark_group("parse_scaling");
    for size in [1, 10, 50, 100] {
        let input: String = (0..size).map(|i| format!("echo line_{i}\n")).collect();
        group.bench_with_input(BenchmarkId::from_parameter(size), &input, |b, input| {
            b.iter(|| wasmsh_parse::parse(black_box(input.as_str())));
        });
    }
    group.finish();
}

criterion_group!(
    benches,
    bench_parse_simple,
    bench_parse_pipeline,
    bench_parse_complex_script,
    bench_parse_scaling
);
criterion_main!(benches);
