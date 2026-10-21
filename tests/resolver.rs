use std::sync::Arc;
use veloci::repo::{MemoryRepo, Package, Repository};
use veloci::resolver;

fn pkg(name: &str, deps: &[&str], provides: &[&str]) -> Package {
    pkgv(name, deps, "1", provides)
}

fn pkgv(name: &str, deps: &[&str], version: &str, provides: &[&str]) -> Package {
    Package {
        name: name.to_string(),
        version: version.to_string(),
        deps: deps.iter().map(|s| s.to_string()).collect(),
        provides: provides.iter().map(|s| s.to_string()).collect(),
        conflicts: vec![],
        url: format!("http://example.invalid/{name}.pkg"),
        size: 1,
        sha256: String::new(),
        mirror: None,
        bins: Vec::new(),
        strip_prefix: None,
        description: None,
    }
}

fn repo_of(pkgs: Vec<Package>) -> Arc<dyn Repository> {
    let mut r = MemoryRepo::new();
    for p in pkgs {
        r.add(p);
    }
    Arc::new(r)
}

async fn resolve(pkgs: Vec<Package>, roots: &[&str]) -> veloci::resolver::Resolution {
    let repo = repo_of(pkgs);
    let roots: Vec<String> = roots.iter().map(|s| s.to_string()).collect();
    resolver::resolve(&roots, repo).await.unwrap()
}

#[tokio::test]
async fn resolves_transitive_dependencies() {
    let res = resolve(
        vec![
            pkg("a", &["b", "c"], &[]),
            pkg("b", &["d"], &[]),
            pkg("c", &["d"], &[]),
            pkg("d", &[], &[]),
        ],
        &["a"],
    )
    .await;

    assert_eq!(res.order.len(), 4);
    let pos = |n: &str| res.order.iter().position(|x| x == n).unwrap();
    assert!(pos("d") < pos("b"));
    assert!(pos("b") < pos("a"));
    assert!(pos("c") < pos("a"));
    assert!(res.verify().is_empty());
}

#[tokio::test]
async fn dependencies_come_before_dependents() {
    let res = resolve(
        vec![
            pkg("app", &["lib"], &[]),
            pkg("lib", &["base"], &[]),
            pkg("base", &[], &[]),
        ],
        &["app"],
    )
    .await;

    assert_eq!(res.order, vec!["base", "lib", "app"]);
    assert!(res.verify().is_empty());
}

#[tokio::test]
async fn virtual_dependencies_resolve_to_their_provider() {
    let res = resolve(
        vec![
            pkg("app", &["libfoo.so"], &[]),
            pkg("foo", &[], &["libfoo.so"]),
        ],
        &["app"],
    )
    .await;

    assert_eq!(res.order, vec!["foo", "app"]);
    assert!(res.verify().is_empty());
}

#[tokio::test]
async fn a_virtual_dependency_does_not_cause_an_endless_loop() {
    let res = resolve(
        vec![
            pkg("app", &["libfoo.so"], &[]),
            pkg("foo", &["base"], &["libfoo.so"]),
            pkg("base", &[], &[]),
        ],
        &["app"],
    )
    .await;

    assert_eq!(res.order.len(), 3);
    assert!(res.verify().is_empty());
}

#[tokio::test]
async fn a_provider_keeps_its_own_dependencies() {
    let res = resolve(
        vec![
            pkg("app", &["libfoo.so"], &[]),
            pkg("foo", &["base"], &["libfoo.so"]),
            pkg("base", &[], &[]),
        ],
        &["app"],
    )
    .await;

    assert!(res.order.contains(&"base".to_string()));
}

#[tokio::test]
async fn cycles_are_broken_rather_than_failing() {
    let res = resolve(
        vec![
            pkg("mesa", &["libglvnd"], &[]),
            pkg("libglvnd", &["mesa"], &[]),
        ],
        &["mesa"],
    )
    .await;

    assert_eq!(res.order.len(), 2);

    assert_eq!(res.cycles.len(), 1);
    assert!(res.verify().is_empty(), "violations: {:?}", res.verify());
}

#[tokio::test]
async fn a_broken_cycle_is_reported_and_not_counted_as_a_violation() {
    let res = resolve(
        vec![
            pkg("librsvg", &["gdk-pixbuf2"], &[]),
            pkg("gdk-pixbuf2", &["glycin"], &[]),
            pkg("glycin", &["librsvg"], &[]),
        ],
        &["librsvg"],
    )
    .await;

    assert_eq!(res.order.len(), 3);

    assert_eq!(res.cycles.len(), 1);
    assert!(res.verify().is_empty(), "violations: {:?}", res.verify());
}

#[tokio::test]
async fn a_cycle_break_keeps_unrelated_packages_ordered() {
    let res = resolve(
        vec![
            pkg("mesa", &["libglvnd"], &[]),
            pkg("libglvnd", &["mesa"], &[]),
            pkg("app", &["libglvnd"], &[]),
        ],
        &["app"],
    )
    .await;

    assert_eq!(res.order.len(), 3);

    let bad = res.verify();
    assert!(bad.is_empty(), "unexpected violations: {bad:?}");
    assert_eq!(res.cycles.len(), 1);
}

#[tokio::test]
async fn a_shared_dependency_is_resolved_once() {
    let res = resolve(
        vec![
            pkg("a", &["shared"], &[]),
            pkg("b", &["shared"], &[]),
            pkg("shared", &[], &[]),
        ],
        &["a", "b"],
    )
    .await;

    assert_eq!(res.order.len(), 3);
    assert_eq!(
        res.order.iter().filter(|n| *n == "shared").count(),
        1,
        "shared dependency resolved more than once"
    );
}

#[tokio::test]
async fn a_missing_package_is_an_error() {
    let repo = repo_of(vec![pkg("a", &["missing"], &[])]);
    let roots = vec!["a".to_string()];
    let err = resolver::resolve(&roots, repo).await.unwrap_err();
    assert!(err.to_string().contains("missing"));
}

#[tokio::test]
async fn a_multilib_provider_does_not_create_a_cycle() {
    let res = resolve(
        vec![
            pkgv("krb5", &["libverto.so"], "1.22.2-1.1", &["libverto.so"]),
            pkgv("lib32-krb5", &["krb5"], "1.22.2-1", &["libverto.so"]),
        ],
        &["krb5", "lib32-krb5"],
    )
    .await;

    let bad = res.verify();
    assert!(bad.is_empty(), "unexpected violations: {bad:?}");

    assert_eq!(
        res.provider_of.get("libverto.so"),
        Some(&"krb5".to_string()),
        "wrong provider: {:?}",
        res.provider_of.get("libverto.so")
    );
}

#[tokio::test]
async fn the_newest_provider_owns_a_shared_soname() {
    let res = resolve(
        vec![
            pkgv("old", &[], "1.0-1", &["libfoo.so"]),
            pkgv("new", &[], "2.0-1", &["libfoo.so"]),
            pkgv("user", &["libfoo.so"], "1.0-1", &[]),
        ],
        &["user", "old", "new"],
    )
    .await;

    assert_eq!(res.provider_of.get("libfoo.so"), Some(&"new".to_string()));
    assert!(res.verify().is_empty(), "{:?}", res.verify());
}

#[tokio::test]
async fn resolution_is_deterministic() {
    let pkgs = vec![
        pkg("a", &["b", "c", "d"], &[]),
        pkg("b", &["e"], &[]),
        pkg("c", &["e"], &[]),
        pkg("d", &["e"], &[]),
        pkg("e", &[], &[]),
    ];
    let first = resolve(pkgs.clone(), &["a"]).await;
    for _ in 0..5 {
        let again = resolve(pkgs.clone(), &["a"]).await;
        assert_eq!(first.order, again.order);
    }
}
