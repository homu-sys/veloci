use veloci::index::Index;

#[test]
fn reads_the_canonical_shape() {
    let json = r#"{
        "packages": [
            {
                "name": "popt",
                "version": "1.19-2",
                "deps": ["glibc"],
                "provides": ["libpopt.so=0-64"],
                "conflicts": [],
                "url": "https://example.org/core/os/x86_64/popt-1.19-2-x86_64.pkg.tar.zst",
                "size": 78173,
                "sha256": "abc123"
            }
        ]
    }"#;

    let index = Index::from_json(json.as_bytes()).unwrap();
    let pkg = index.packages().get("popt").expect("popt");
    assert_eq!(pkg.version, "1.19-2");
    assert_eq!(pkg.deps, vec!["glibc"]);
    assert_eq!(pkg.provides, vec!["libpopt.so=0-64"]);
    assert_eq!(pkg.size, 78173);
    assert_eq!(pkg.sha256, "abc123");
}

#[test]
fn reads_a_bare_array() {
    let json = r#"[{"name": "a", "version": "1", "url": "https://example.org/a"}]"#;
    let index = Index::from_json(json.as_bytes()).unwrap();
    assert!(index.packages().contains_key("a"));
}

#[test]
fn reads_the_field_names_other_tools_use() {
    let json = r#"{
        "packages": [
            {
                "pkgname": "widget",
                "pkgver": "2.0",
                "dependencies": ["libc", "zlib"],
                "requires": ["ignored"],
                "download_url": "https://example.org/widget-2.0.tar.gz",
                "csize": 4096,
                "sha256sum": "deadbeef"
            }
        ]
    }"#;

    let index = Index::from_json(json.as_bytes()).unwrap();
    let pkg = index.packages().get("widget").expect("widget");
    assert_eq!(pkg.version, "2.0");
    assert_eq!(pkg.deps, vec!["libc", "zlib"]);
    assert_eq!(pkg.url, "https://example.org/widget-2.0.tar.gz");
    assert_eq!(pkg.size, 4096);
    assert_eq!(pkg.sha256, "deadbeef");
}

#[test]
fn reads_a_package_wrapped_in_an_envelope() {
    let json = r#"[
        {
            "package": {"name": "abtop", "version": "0.5.5"},
            "archive": {
                "file": "abtop-x86_64-unknown-linux-gnu.tar.xz",
                "url": "https://example.org/abtop-0.5.5.tar.xz"
            }
        }
    ]"#;

    let index = Index::from_json(json.as_bytes()).unwrap();
    let pkg = index.packages().get("abtop").expect("abtop");
    assert_eq!(pkg.version, "0.5.5");
    assert_eq!(pkg.url, "https://example.org/abtop-0.5.5.tar.xz");
}

#[test]
fn reads_a_depends_field_as_one_string() {
    let json = r#"{"packages": [{"name": "a", "version": "1", "deps": "b c"}]}"#;
    let index = Index::from_json(json.as_bytes()).unwrap();
    let pkg = index.packages().get("a").expect("a");
    assert_eq!(pkg.deps, vec!["b", "c"]);
}

#[test]
fn reads_mirror_sets() {
    let json = r#"{
        "mirrors": [["https://one.example.org/{file}", "https://two.example.org/{file}"]],
        "packages": [
            {"name": "a", "version": "1", "url": "https://one.example.org/a.pkg", "mirror": 0}
        ]
    }"#;

    let index = Index::from_json(json.as_bytes()).unwrap();
    assert_eq!(index.mirrors().len(), 1);
    assert_eq!(index.mirrors()[0].len(), 2);
    let pkg = index.packages().get("a").expect("a");
    assert_eq!(pkg.mirror, Some(0));
}

#[test]
fn a_packed_index_round_trips() {
    let json = r#"{
        "mirrors": [["https://one.example.org/{file}"]],
        "packages": [
            {
                "name": "a",
                "version": "1",
                "deps": ["b"],
                "provides": ["liba.so=1"],
                "conflicts": ["old-a"],
                "url": "https://one.example.org/a.pkg",
                "size": 10,
                "sha256": "abc",
                "mirror": 0
            }
        ]
    }"#;

    let index = Index::from_json(json.as_bytes()).unwrap();
    let pkgs: Vec<_> = index.packages().values().cloned().collect();
    let dir = std::env::temp_dir().join("veloci-index-roundtrip");
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("index.veloci");
    Index::write_binary_with_mirrors(&path, &pkgs, index.mirrors()).unwrap();

    let bytes = std::fs::read(&path).unwrap();
    let again = Index::from_binary(&bytes).unwrap();
    let pkg = again.packages().get("a").expect("a");
    assert_eq!(pkg.deps, vec!["b"]);
    assert_eq!(pkg.provides, vec!["liba.so=1"]);
    assert_eq!(pkg.conflicts, vec!["old-a"]);
    assert_eq!(pkg.size, 10);
    assert_eq!(pkg.sha256, "abc");
    assert_eq!(pkg.mirror, Some(0));
    assert_eq!(again.mirrors(), index.mirrors());
    let _ = std::fs::remove_dir_all(&dir);
}
