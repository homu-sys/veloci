use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;
use veloci::repo::Package;
use veloci::{download::Downloader, resolver};

struct TestServer {
    url: String,
    hits: Arc<AtomicUsize>,
    #[allow(dead_code)]
    payload: Vec<u8>,
}

async fn start_server(payload: Vec<u8>) -> TestServer {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let hits = Arc::new(AtomicUsize::new(0));
    let hits_c = Arc::clone(&hits);
    let body = payload.clone();

    tokio::spawn(async move {
        loop {
            let Ok((mut socket, _)) = listener.accept().await else {
                break;
            };
            let body = body.clone();
            let hits = Arc::clone(&hits_c);
            tokio::spawn(async move {
                let mut buf = vec![0u8; 4096];
                let n = socket.read(&mut buf).await.unwrap_or(0);
                let req = String::from_utf8_lossy(&buf[..n]).to_string();
                hits.fetch_add(1, Ordering::SeqCst);

                let range = req
                    .lines()
                    .find(|l| l.to_ascii_lowercase().starts_with("range:"))
                    .and_then(|l| l.split('=').nth(1))
                    .and_then(|s| s.split('-').next())
                    .and_then(|s| s.trim().parse::<usize>().ok());

                let (status, slice) = match range {
                    Some(start) if start < body.len() => {
                        ("206 Partial Content", body[start..].to_vec())
                    }
                    _ => ("200 OK", body.clone()),
                };

                let header = format!(
                    "HTTP/1.1 {status}\r\nContent-Length: {}\r\nAccept-Ranges: bytes\r\nConnection: close\r\n\r\n",
                    slice.len()
                );
                let _ = socket.write_all(header.as_bytes()).await;
                let _ = socket.write_all(&slice).await;
                let _ = socket.flush().await;
            });
        }
    });

    TestServer {
        url: format!("http://{addr}/pkg.pkg.tar.zst"),
        hits,
        payload,
    }
}

fn pkg(url: &str, sha256: &str) -> Package {
    Package {
        name: "demo".to_string(),
        version: "1".to_string(),
        deps: vec![],
        provides: vec![],
        conflicts: vec![],
        url: url.to_string(),
        size: 0,
        sha256: sha256.to_string(),
        mirror: None,
        bins: Vec::new(),
        strip_prefix: None,
        description: None,
    }
}

fn sha256_hex(data: &[u8]) -> String {
    use sha2::{Digest, Sha256};
    let mut h = Sha256::new();
    h.update(data);
    hex::encode(h.finalize())
}

#[tokio::test]
async fn downloads_and_verifies_a_package() {
    let payload = b"veloci test payload".repeat(500);
    let server = start_server(payload.clone()).await;
    let hash = sha256_hex(&payload);

    let dir = tempdir("dl-verify");
    let dl = Downloader::new(&dir, 4).unwrap();
    let out = dl.fetch_all(&[pkg(&server.url, &hash)]).await.unwrap();

    assert_eq!(out.len(), 1);
    assert_eq!(std::fs::read(&out[0]).unwrap(), payload);

    assert!(out[0]
        .file_name()
        .unwrap()
        .to_string_lossy()
        .contains("pkg.pkg.tar.zst"));
}

#[tokio::test]
async fn a_cached_package_is_not_downloaded_again() {
    let payload = b"veloci cache payload".repeat(200);
    let server = start_server(payload.clone()).await;
    let hash = sha256_hex(&payload);

    let dir = tempdir("dl-cache");
    let dl = Downloader::new(&dir, 4).unwrap();
    let p = pkg(&server.url, &hash);

    dl.fetch_all(std::slice::from_ref(&p)).await.unwrap();
    assert_eq!(server.hits.load(Ordering::SeqCst), 1);

    dl.fetch_all(std::slice::from_ref(&p)).await.unwrap();
    assert_eq!(
        server.hits.load(Ordering::SeqCst),
        1,
        "cached package was fetched again"
    );
}

#[tokio::test]
async fn a_checksum_mismatch_is_rejected() {
    let payload = b"veloci mismatch payload".repeat(50);
    let server = start_server(payload).await;

    let dir = tempdir("dl-badsum");
    let dl = Downloader::new(&dir, 4).unwrap();
    let err = dl
        .fetch_all(&[pkg(&server.url, &"00".repeat(32))])
        .await
        .unwrap_err();
    assert!(err.to_string().contains("checksum mismatch"), "{err}");

    let leftovers: Vec<_> = std::fs::read_dir(&dir)
        .unwrap()
        .filter_map(|e| e.ok())
        .map(|e| e.file_name().to_string_lossy().to_string())
        .filter(|n| !n.ends_with(".sha256"))
        .collect();
    assert!(leftovers.is_empty(), "unexpected files: {leftovers:?}");
}

#[tokio::test]
async fn a_corrupt_cached_artifact_is_refetched() {
    let payload = b"veloci corrupt payload".repeat(100);
    let server = start_server(payload.clone()).await;
    let hash = sha256_hex(&payload);

    let dir = tempdir("dl-corrupt");
    let dl = Downloader::new(&dir, 4).unwrap();
    let p = pkg(&server.url, &hash);

    let out = dl.fetch_all(std::slice::from_ref(&p)).await.unwrap();
    assert_eq!(server.hits.load(Ordering::SeqCst), 1);

    std::fs::write(&out[0], b"garbage").unwrap();

    dl.fetch_all(std::slice::from_ref(&p)).await.unwrap();

    assert_eq!(std::fs::read(&out[0]).unwrap(), payload);
}

#[tokio::test]
async fn packages_are_downloaded_concurrently() {
    let payload = b"veloci parallel payload".repeat(100);
    let server = start_server(payload.clone()).await;
    let hash = sha256_hex(&payload);

    let dir = tempdir("dl-parallel");
    let dl = Downloader::new(&dir, 8).unwrap();

    let pkgs: Vec<Package> = (0..16)
        .map(|i| {
            let mut p = pkg(&server.url, &hash);
            p.name = format!("demo{i}");
            p
        })
        .collect();

    let out = dl.fetch_all(&pkgs).await.unwrap();
    assert_eq!(out.len(), 16);
    for o in &out {
        assert_eq!(std::fs::read(o).unwrap(), payload);
    }
}

#[tokio::test]
async fn an_interrupted_download_resumes_instead_of_restarting() {
    let payload = b"veloci resume payload".repeat(400);
    let server = start_server(payload.clone()).await;
    let hash = sha256_hex(&payload);

    let dir = tempdir("dl-resume");
    let dl = Downloader::new(&dir, 4).unwrap();

    let split = payload.len() / 2;
    let part = dir.join("pkg.pkg.tar.zst.part");
    std::fs::write(&part, &payload[..split]).unwrap();

    let out = dl.fetch_all(&[pkg(&server.url, &hash)]).await.unwrap();

    assert_eq!(std::fs::read(&out[0]).unwrap(), payload);
    assert!(!part.exists(), "partial file was left behind");

    let marker = std::fs::read_to_string(dir.join("pkg.pkg.tar.zst.sha256")).unwrap();
    assert!(marker.starts_with(&hash), "marker holds {marker}");
}

#[tokio::test]
async fn packages_sharing_a_url_basename_do_not_collide() {
    let payload = b"veloci collision payload".repeat(50);
    let server = start_server(payload.clone()).await;
    let hash = sha256_hex(&payload);

    let dir = tempdir("dl-collide");
    let dl = Downloader::new(&dir, 4).unwrap();

    let pkgs: Vec<Package> = (0..4)
        .map(|i| {
            let mut p = pkg(&server.url, &hash);
            p.name = format!("demo{i}");
            p
        })
        .collect();

    let out = dl.fetch_all(&pkgs).await.unwrap();
    assert_eq!(out.len(), 4);

    let mut names: Vec<String> = out
        .iter()
        .map(|p| p.file_name().unwrap().to_string_lossy().to_string())
        .collect();
    names.sort();
    names.dedup();
    assert_eq!(names.len(), 4, "artifacts overwrote each other: {names:?}");

    for o in &out {
        assert_eq!(std::fs::read(o).unwrap(), payload);
    }
}

#[tokio::test]
async fn the_resolver_and_downloader_work_together() {
    let payload = b"veloci end to end".repeat(20);
    let server = start_server(payload.clone()).await;
    let hash = sha256_hex(&payload);

    let mut repo = veloci::repo::MemoryRepo::new();
    repo.add(Package {
        name: "app".to_string(),
        version: "1".to_string(),
        deps: vec!["lib".to_string()],
        provides: vec![],
        conflicts: vec![],
        url: server.url.clone(),
        size: payload.len() as u64,
        sha256: hash.clone(),
        mirror: None,
        bins: Vec::new(),
        strip_prefix: None,
        description: None,
    });
    repo.add(Package {
        name: "lib".to_string(),
        version: "1".to_string(),
        deps: vec![],
        provides: vec![],
        conflicts: vec![],
        url: server.url.clone(),
        size: payload.len() as u64,
        sha256: hash.clone(),
        mirror: None,
        bins: Vec::new(),
        strip_prefix: None,
        description: None,
    });

    let repo: Arc<dyn veloci::repo::Repository> = Arc::new(repo);
    let res = resolver::resolve(&["app".to_string()], Arc::clone(&repo))
        .await
        .unwrap();
    assert_eq!(res.order, vec!["lib", "app"]);

    let dir = tempdir("dl-e2e");
    let dl = Downloader::new(&dir, 4).unwrap();
    let out = dl.fetch_all(&res.packages).await.unwrap();
    assert_eq!(out.len(), 2);
}

#[tokio::test]
async fn a_broken_mirror_falls_back_to_the_next_one() {
    let payload = b"veloci mirror fallback".repeat(10);
    let server = start_server(payload.clone()).await;
    let hash = sha256_hex(&payload);

    let mut package = pkg("http://127.0.0.1:1/missing.pkg", &hash);
    package.size = payload.len() as u64;
    package.mirror = Some(0);

    let dir = tempdir("dl-mirror");

    let mirrors = vec![vec![server.url.clone()]];
    let dl = Downloader::with_mirrors(&dir, 4, mirrors).unwrap();

    let out = dl.fetch_all(&[package]).await.unwrap();
    assert_eq!(out.len(), 1);
    assert_eq!(std::fs::read(&out[0]).unwrap(), payload);
}

fn tempdir(tag: &str) -> std::path::PathBuf {
    use std::sync::atomic::AtomicU64;
    static N: AtomicU64 = AtomicU64::new(0);
    let n = N.fetch_add(1, Ordering::SeqCst);
    let dir = std::env::temp_dir().join(format!("veloci-test-{tag}-{}-{n}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}
