use criterion::{black_box, criterion_group, criterion_main, BenchmarkId, Criterion};
use wasmsh_browser::WorkerRuntime;
use wasmsh_protocol::HostCommand;

fn bench_full_pipeline(c: &mut Criterion) {
    let mut group = c.benchmark_group("full_pipeline");

    let scripts = [
        ("echo", "echo hello world"),
        ("pipeline", "echo abc | cat | cat"),
        ("loop_10", "for i in 1 2 3 4 5 6 7 8 9 10; do echo $i; done"),
        (
            "arithmetic",
            "i=0; while [ $i -lt 10 ]; do echo $((i*i)); i=$((i+1)); done",
        ),
    ];

    for (name, script) in scripts {
        group.bench_with_input(BenchmarkId::from_parameter(name), &script, |b, &script| {
            b.iter_custom(|iters| {
                let start = std::time::Instant::now();
                for _ in 0..iters {
                    let mut rt = WorkerRuntime::new();
                    rt.handle_command(HostCommand::Init {
                        step_budget: 100_000,
                    });
                    let _ = rt.handle_command(HostCommand::Run {
                        input: black_box(script).into(),
                    });
                }
                start.elapsed()
            });
        });
    }
    group.finish();
}

fn bench_init_overhead(c: &mut Criterion) {
    c.bench_function("runtime_init", |b| {
        b.iter(|| {
            let mut rt = WorkerRuntime::new();
            rt.handle_command(HostCommand::Init {
                step_budget: 100_000,
            });
        });
    });
}

criterion_group!(benches, bench_full_pipeline, bench_init_overhead);
criterion_main!(benches);
