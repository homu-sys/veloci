use crate::repo::Package;
use anyhow::{bail, Context, Result};
use futures::StreamExt;
use indicatif::{ProgressBar, ProgressStyle};
use sha2::{Digest, Sha256};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use tokio::io::AsyncWriteExt;

pub struct Downloader {
    client: reqwest::Client,
    cache: PathBuf,
    concurrency: usize,

    mirrors: Vec<Vec<String>>,
}

impl Downloader {
    pub fn new(cache: impl Into<PathBuf>, concurrency: usize) -> Result<Self> {
        Self::with_mirrors(cache, concurrency, Vec::new())
    }

    pub fn with_mirrors(
        cache: impl Into<PathBuf>,
        concurrency: usize,
        mirrors: Vec<Vec<String>>,
    ) -> Result<Self> {
        let cache = cache.into();
        std::fs::create_dir_all(&cache).ok();
        let client = reqwest::Client::builder()
            .pool_max_idle_per_host(concurrency)
            .tcp_nodelay(true)
            .user_agent(concat!("veloci/", env!("CARGO_PKG_VERSION")))
            .build()?;
        Ok(Self {
            client,
            cache,
            concurrency,
            mirrors,
        })
    }

    pub async fn fetch_all(&self, pkgs: &[Package]) -> Result<Vec<PathBuf>> {
        let sem = Arc::new(tokio::sync::Semaphore::new(self.concurrency));
        let total: u64 = pkgs.iter().map(|p| p.size).sum();
        let bar = ProgressBar::new(total);
        bar.set_style(
            ProgressStyle::with_template(
                "{bar:40.cyan/blue} {bytes}/{total_bytes} ({bytes_per_sec}, {eta}) {pos}/{len}",
            )
            .unwrap_or_else(|_| ProgressStyle::default_bar())
            .progress_chars("=> "),
        );
        let bar = Arc::new(bar);

        let mut tasks = Vec::with_capacity(pkgs.len());
        let names = assign_artifact_names(pkgs);

        for (pkg, artifact) in pkgs.iter().zip(names) {
            let pkg = pkg.clone();
            let client = self.client.clone();
            let sem = Arc::clone(&sem);
            let cache = self.cache.clone();
            let bar = Arc::clone(&bar);
            let mirrors = Arc::new(self.mirrors.clone());
            tasks.push(tokio::spawn(async move {
                let _permit = sem.acquire_owned().await?;
                fetch_one(&client, &cache, &pkg, &artifact, &bar, &mirrors).await
            }));
        }

        let mut results = Vec::with_capacity(tasks.len());
        let mut first_err = None;
        for t in tasks {
            match t.await {
                Ok(Ok(p)) => results.push(p),
                Ok(Err(e)) => {
                    if first_err.is_none() {
                        first_err = Some(e);
                    }
                }
                Err(e) => {
                    if first_err.is_none() {
                        first_err = Some(e.into());
                    }
                }
            }
        }
        bar.finish_and_clear();

        match first_err {
            Some(e) => Err(e),
            None => Ok(results),
        }
    }
}

fn artifact_name(pkg: &Package) -> String {
    let from_url = pkg
        .url
        .split(['?', '#'])
        .next()
        .unwrap_or("")
        .rsplit('/')
        .next()
        .filter(|s| !s.is_empty());
    match from_url {
        Some(n) if n.contains('.') => n.to_string(),
        _ => format!("{}-{}.pkg", pkg.name, pkg.version),
    }
}

fn assign_artifact_names(pkgs: &[Package]) -> Vec<String> {
    let mut used: HashMap<String, usize> = HashMap::new();
    let mut names = Vec::with_capacity(pkgs.len());
    for pkg in pkgs {
        let base = artifact_name(pkg);
        let count = used.entry(base.clone()).or_insert(0);
        *count += 1;
        if *count == 1 {
            names.push(base);
        } else {
            names.push(format!("{}-{}", pkg.name, base));
        }
    }
    names
}

fn marker_path(dest: &Path) -> PathBuf {
    let mut name = dest.file_name().unwrap_or_default().to_os_string();
    name.push(".sha256");
    dest.with_file_name(name)
}

async fn read_marker(dest: &Path) -> Option<(String, u64)> {
    let text = tokio::fs::read_to_string(marker_path(dest)).await.ok()?;
    let mut parts = text.split_whitespace();
    let hash = parts.next()?.to_string();
    let size = parts.next().and_then(|s| s.parse().ok()).unwrap_or(0);
    Some((hash, size))
}

async fn write_marker(dest: &Path, hash: &str, size: u64) -> Result<()> {
    tokio::fs::write(marker_path(dest), format!("{hash} {size}"))
        .await
        .ok();
    Ok(())
}

pub async fn fetch_bytes(url: &str) -> Result<Vec<u8>> {
    let client = reqwest::Client::builder()
        .user_agent(concat!("veloci/", env!("CARGO_PKG_VERSION")))
        .build()?;
    let resp = client
        .get(url)
        .send()
        .await
        .with_context(|| format!("fetching {url}"))?
        .error_for_status()
        .with_context(|| format!("fetching {url}"))?;
    let bytes = resp
        .bytes()
        .await
        .with_context(|| format!("reading {url}"))?;
    Ok(bytes.to_vec())
}

pub async fn remote_sizes(urls: &[String]) -> Vec<Option<u64>> {
    let client = match reqwest::Client::builder()
        .user_agent(concat!("veloci/", env!("CARGO_PKG_VERSION")))
        .build()
    {
        Ok(client) => client,
        Err(_) => return vec![None; urls.len()],
    };

    let requests = urls.iter().map(|url| {
        let client = client.clone();
        let url = url.clone();
        async move {
            client.head(&url).send().await.ok().and_then(|resp| {
                resp.headers()
                    .get(reqwest::header::CONTENT_LENGTH)?
                    .to_str()
                    .ok()?
                    .parse::<u64>()
                    .ok()
            })
        }
    });

    futures::future::join_all(requests).await
}

fn candidate_urls(pkg: &Package, mirrors: &[Vec<String>]) -> Vec<String> {
    let mut urls = vec![pkg.url.clone()];
    let Some(set) = pkg.mirror.and_then(|id| mirrors.get(id as usize)) else {
        return urls;
    };
    let Some(filename) = pkg
        .url
        .split(['?', '#'])
        .next()
        .unwrap_or("")
        .rsplit('/')
        .next()
        .filter(|s| !s.is_empty())
    else {
        return urls;
    };
    for template in set {
        let candidate = template.replace("{file}", filename);
        if !candidate.is_empty() && !urls.contains(&candidate) {
            urls.push(candidate);
        }
    }
    urls
}

async fn fetch_one(
    client: &reqwest::Client,
    cache: &Path,
    pkg: &Package,
    artifact: &str,
    bar: &ProgressBar,
    mirrors: &[Vec<String>],
) -> Result<PathBuf> {
    let dest = cache.join(artifact);
    let expected = pkg.sha256.to_lowercase();

    if dest.exists() {
        if !expected.is_empty() {
            if let Some((hash, size)) = read_marker(&dest).await {
                let on_disk = tokio::fs::metadata(&dest)
                    .await
                    .map(|m| m.len())
                    .unwrap_or(0);
                if hash == expected && size == on_disk {
                    bar.inc(pkg.size);
                    return Ok(dest);
                }
            }
        }

        if let Ok(hash) = hash_file(&dest).await {
            let size = tokio::fs::metadata(&dest)
                .await
                .map(|m| m.len())
                .unwrap_or(0);
            if expected.is_empty() || hash == expected {
                write_marker(&dest, &hash, size).await?;
                bar.inc(pkg.size);
                return Ok(dest);
            }
        }

        tokio::fs::remove_file(&dest).await.ok();
    }

    let part = cache.join(format!("{artifact}.part"));
    let urls = candidate_urls(pkg, mirrors);

    let mut last_err = None;
    for url in &urls {
        match fetch_from(client, cache, pkg, artifact, bar, &part, url).await {
            Ok(path) => return Ok(path),
            Err(e) => last_err = Some(e),
        }
    }

    Err(last_err.unwrap_or_else(|| anyhow::anyhow!("no url for {}", pkg.name)))
}

async fn fetch_from(
    client: &reqwest::Client,
    cache: &Path,
    pkg: &Package,
    artifact: &str,
    bar: &ProgressBar,
    part: &Path,
    url: &str,
) -> Result<PathBuf> {
    let dest = cache.join(artifact);
    let expected = pkg.sha256.to_lowercase();

    let mut request = client.get(url);

    let mut resume_from = 0u64;
    if let Ok(meta) = tokio::fs::metadata(part).await {
        let len = meta.len();
        if len > 0 && (pkg.size == 0 || len < pkg.size) {
            resume_from = len;
            request = request.header(reqwest::header::RANGE, format!("bytes={len}-"));
        }
    }

    let resp = request
        .send()
        .await
        .with_context(|| format!("fetching {url}"))?
        .error_for_status()
        .with_context(|| format!("fetching {url}"))?;

    let resuming = resp.status() == reqwest::StatusCode::PARTIAL_CONTENT;
    let mut file = if resuming && resume_from > 0 {
        tokio::fs::OpenOptions::new()
            .append(true)
            .open(part)
            .await
            .with_context(|| format!("resuming {}", part.display()))?
    } else {
        tokio::fs::File::create(part)
            .await
            .with_context(|| format!("creating {}", part.display()))?
    };

    let mut hasher = Sha256::new();

    if resuming && resume_from > 0 {
        hash_file_into(part, &mut hasher).await?;
    }

    let mut stream = resp.bytes_stream();
    while let Some(chunk) = stream.next().await {
        let chunk = chunk?;
        hasher.update(&chunk);
        file.write_all(&chunk).await?;
        bar.inc(chunk.len() as u64);
    }
    file.flush().await?;
    drop(file);

    let got = hex::encode(hasher.finalize());
    if !expected.is_empty() && got != expected {
        tokio::fs::remove_file(part).await.ok();
        bail!(
            "checksum mismatch for {}: expected {}, got {}",
            pkg.name,
            pkg.sha256,
            got
        );
    }

    tokio::fs::rename(part, &dest).await?;
    if !expected.is_empty() {
        let size = tokio::fs::metadata(&dest)
            .await
            .map(|m| m.len())
            .unwrap_or(0);
        write_marker(&dest, &got, size).await?;
    }
    Ok(dest)
}

async fn hash_file(path: &Path) -> Result<String> {
    let mut hasher = Sha256::new();
    hash_file_into(path, &mut hasher).await?;
    Ok(hex::encode(hasher.finalize()))
}

async fn hash_file_into(path: &Path, hasher: &mut Sha256) -> Result<u64> {
    use tokio::io::AsyncReadExt;
    let mut file = tokio::fs::File::open(path).await?;
    let mut buf = vec![0u8; 128 * 1024];
    let mut total = 0u64;
    loop {
        let n = file.read(&mut buf).await?;
        if n == 0 {
            break;
        }
        hasher.update(&buf[..n]);
        total += n as u64;
    }
    Ok(total)
}
