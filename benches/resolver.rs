use criterion::{criterion_group, criterion_main, Criterion};
use std::sync::Arc;
use veloci::repo::{MemoryRepo, Package, Repository};
use veloci::resolver;

fn synthetic_repo(size: usize) -> Arc<dyn Repository> {
    let mut repo = MemoryRepo::new();

    for i in 0..size {
        let mut deps = Vec::new();
        if i > 0 {
            deps.push(format!("pkg{i}"));
        }

        for j in 0..3 {
            let dep = (i * 7 + j * 13) % (i + 1);
            if dep != i {
                deps.push(format!("pkg{dep}"));
            }
        }

        if i + 1 < size && i % 2000 == 0 {
            deps.push(format!("pkg{}", i + 1));
        }

        let provides = if i % 25 == 0 {
            vec![format!("virtual-lib-{}", i / 25)]
        } else {
            vec![]
        };

        repo.add(Package {
            bins: Vec::new(),
            strip_prefix: None,
            description: None,
            name: format!("pkg{i}"),
            version: "1.0".to_string(),
            deps,
            provides,
            conflicts: vec![],
            url: format!("http://example.invalid/pkg{i}.pkg"),
            size: 1024,
            sha256: String::new(),
            mirror: None,
        });
    }

    for v in 0..(size / 25) {
        repo.add(Package {
            bins: Vec::new(),
            strip_prefix: None,
            description: None,
            name: format!("alt-lib-{v}"),
            version: "0.9".to_string(),
            deps: vec![],
            provides: vec![format!("virtual-lib-{v}")],
            conflicts: vec![],
            url: format!("http://example.invalid/alt-lib-{v}.pkg"),
            size: 1024,
            sha256: String::new(),
            mirror: None,
        });
    }

    Arc::new(repo)
}

fn bench_resolve(c: &mut Criterion) {
    let mut group = c.benchmark_group("resolve");
    for size in [1_000usize, 16_000] {
        let repo = synthetic_repo(size);

        let roots = vec![format!("pkg{}", size - 1)];
        group.bench_function(format!("{size}_packages"), |b| {
            b.to_async(tokio::runtime::Runtime::new().unwrap())
                .iter(|| {
                    let repo = Arc::clone(&repo);
                    let roots = roots.clone();
                    async move { resolver::resolve(&roots, repo).await.unwrap() }
                });
        });
    }
    group.finish();
}

fn bench_resolve_realistic_roots(c: &mut Criterion) {
    let repo = synthetic_repo(16_000);
    let roots = ["pkg15999", "pkg12000", "pkg9000", "pkg4000", "pkg100"]
        .iter()
        .map(|s| s.to_string())
        .collect::<Vec<_>>();
    c.bench_function("resolve_16k_multiple_roots", |b| {
        b.to_async(tokio::runtime::Runtime::new().unwrap())
            .iter(|| {
                let repo = Arc::clone(&repo);
                let roots = roots.clone();
                async move { resolver::resolve(&roots, repo).await.unwrap() }
            });
    });
}

criterion_group!(benches, bench_resolve, bench_resolve_realistic_roots);
criterion_main!(benches);
