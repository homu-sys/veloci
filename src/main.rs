use anyhow::Result;
use clap::{Parser, Subcommand};
use std::path::PathBuf;
use std::time::Instant;
use veloci::config;
use veloci::index::Index;
use veloci::repo;
use veloci::{download, format, install, resolver, sync};

#[derive(Parser)]
#[command(name = "velo", version, about = "The fastest package manager.")]
struct Cli {
    #[arg(
        long,
        env = "VELOCI_INDEX",
        help = "The repository index: a path to a JSON or packed index, or a url"
    )]
    index: Option<String>,

    #[arg(
        long,
        env = "VELOCI_CACHE",
        help = "Directory where downloaded packages are cached"
    )]
    cache: Option<PathBuf>,

    #[arg(
        long,
        env = "VELOCI_CONCURRENCY",
        default_value_t = 16,
        help = "Maximum simultaneous downloads"
    )]
    concurrency: usize,

    #[arg(long, help = "Install system-wide")]
    system: bool,

    #[arg(long, help = "Force system-wide install (alias for --system)")]
    sudo: bool,

    #[command(subcommand)]
    cmd: Command,
}

#[derive(Subcommand)]
enum Command {
    #[command(about = "Download the index of every configured repository")]
    Sync {
        #[arg(
            help = "A repository url to use instead of the configured ones. The url is remembered, so the next sync needs no argument"
        )]
        url: Option<String>,
    },

    #[command(about = "Resolve a set of packages and print the install plan")]
    Resolve {
        #[arg(help = "Package names to resolve")]
        packages: Vec<String>,
    },

    #[command(about = "Download packages in parallel without installing")]
    Download {
        #[arg(help = "Package names to download")]
        packages: Vec<String>,
    },

    #[command(about = "Resolve and verify that every dependency precedes its dependents")]
    Verify {
        #[arg(help = "Package names to resolve")]
        packages: Vec<String>,
    },

    #[command(about = "Install packages, and everything they depend on")]
    Install {
        #[arg(help = "Package names to install")]
        packages: Vec<String>,

        #[arg(short, long, help = "Do not ask for a confirmation")]
        yes: bool,

        #[arg(long, help = "The directory packages are unpacked into")]
        root: Option<PathBuf>,

        #[arg(long, help = "The directory the binaries are linked into")]
        bin: Option<PathBuf>,
    },

    #[command(about = "Remove installed packages")]
    Remove {
        #[arg(help = "Package names to remove")]
        packages: Vec<String>,

        #[arg(long, help = "The directory packages are unpacked into")]
        root: Option<PathBuf>,

        #[arg(long, help = "The directory the binaries are linked into")]
        bin: Option<PathBuf>,
    },

    #[command(about = "List the installed packages")]
    List {
        #[arg(help = "Package names to list")]
        packages: Vec<String>,

        #[arg(long, help = "The directory packages are unpacked into")]
        root: Option<PathBuf>,

        #[arg(long, help = "The directory the binaries are linked into")]
        bin: Option<PathBuf>,
    },

    #[command(subcommand, about = "Index maintenance")]
    Index(IndexCmd),
}

#[derive(Subcommand)]
enum IndexCmd {
    #[command(about = "Pack a JSON index into the fast binary format")]
    Pack {
        #[arg(help = "The JSON index to read")]
        input: PathBuf,
        #[arg(help = "The binary index to write")]
        output: PathBuf,
    },

    #[command(about = "Download an index from a url and pack it")]
    Fetch {
        #[arg(help = "The index to download")]
        url: String,
        #[arg(help = "The binary index to write")]
        output: PathBuf,
    },

    #[command(about = "Show what is in an index")]
    Info {
        #[arg(help = "The index to inspect")]
        path: PathBuf,
    },
}

#[tokio::main]
async fn main() -> Result<()> {
    let cli = Cli::parse();

    match cli.cmd {
        Command::Index(cmd) => return index_cmd(cmd).await,
        Command::Sync { url } => return sync_cmd(url).await,
        Command::Install {
            packages,
            root,
            bin,
            yes,
        } => {
            return install_cmd(
                packages,
                root,
                bin,
                cli.index.clone(),
                cli.concurrency,
                yes,
                cli.system || cli.sudo,
            )
            .await
        }
        Command::Remove {
            packages,
            root,
            bin,
        } => return remove_cmd(packages, root, bin, cli.system || cli.sudo),
        Command::List {
            packages,
            root,
            bin,
        } => return list_cmd(packages, root, bin, cli.system || cli.sudo),
        cmd => {
            let t0 = Instant::now();
            let index = load_index(cli.index.as_deref().unwrap_or_default()).await?;
            let index_mirrors = index.mirrors().to_vec();
            let repo: std::sync::Arc<dyn repo::Repository> = std::sync::Arc::new(index);
            eprintln!("loaded index in {}", format::duration(t0.elapsed()));
            let t1 = Instant::now();

            let res = resolver::resolve(&cmd_packages(&cmd), std::sync::Arc::clone(&repo)).await?;
            eprintln!(
                "resolved {} packages in {}",
                res.packages.len(),
                format::duration(t1.elapsed())
            );

            match cmd {
                Command::Resolve { .. } => {
                    for name in &res.order {
                        println!("{name}");
                    }
                }
                Command::Verify { .. } => {
                    let bad = res.verify();
                    for cycle in &res.cycles {
                        println!("CYCLE: {} (broken)", cycle.join(" -> "));
                    }
                    if bad.is_empty() {
                        println!(
                            "OK: {} packages, dependency order verified",
                            res.order.len()
                        );
                    } else {
                        for (p, d) in &bad {
                            println!("VIOLATION: {p} depends on {d} which comes later");
                        }
                        anyhow::bail!("{} ordering violations", bad.len());
                    }
                }
                Command::Sync { .. } => unreachable!("handled above"),
                Command::Install { .. } | Command::Remove { .. } | Command::List { .. } => {
                    unreachable!("handled above")
                }
                Command::Download { .. } => {
                    let dl = download::Downloader::with_mirrors(
                        cache_dir(),
                        cli.concurrency,
                        index_mirrors.clone(),
                    )?;
                    let t2 = Instant::now();
                    let paths = dl.fetch_all(&res.packages).await?;
                    eprintln!(
                        "downloaded {} packages in {}",
                        paths.len(),
                        format::duration(t2.elapsed())
                    );
                    for p in paths {
                        println!("{}", p.display());
                    }
                }
                Command::Index(_) => unreachable!(),
            }
        }
    }

    Ok(())
}

async fn load_index(source: &str) -> Result<Index> {
    if source.starts_with("http://") || source.starts_with("https://") {
        let bytes = download::fetch_bytes(source).await?;
        return Index::from_json(&bytes);
    }
    if !source.is_empty() {
        return Index::load(std::path::Path::new(source));
    }

    let synced = config::index_path();
    if synced.exists() {
        return Index::load(&synced);
    }
    let local = std::path::Path::new("index.json");
    if local.exists() {
        return Index::load(local);
    }
    anyhow::bail!("no index found. Run `velo sync <url>` to fetch one, or pass --index <path>")
}

fn cache_dir() -> PathBuf {
    config::package_cache()
}

async fn sync_cmd(url: Option<String>) -> Result<()> {
    let mut config = config::Config::load()?;

    if let Some(url) = url {
        let name = config::repo_name_from_url(&url);
        config.set_repo(config::Repo { name, url });
        config.save()?;
        eprintln!(
            "saved {} to {}",
            config.repo.last().unwrap().url,
            config::Config::path().display()
        );
    }

    if config.repo.is_empty() {
        anyhow::bail!(
            "no repositories configured. Run `velo sync <url>`, or edit {}",
            config::Config::path().display()
        );
    }

    let dest = config::index_path();
    let t0 = Instant::now();
    let report = sync::sync(&config, &dest).await?;

    println!(
        "synced {} packages from {} repositories in {:?}",
        report.total,
        config.repo.len(),
        t0.elapsed()
    );
    println!(
        "{} added, {} updated, {} removed, {} unchanged",
        report.added.len(),
        report.updated.len(),
        report.removed.len(),
        report.unchanged
    );
    for (name, old, new) in report.updated.iter().take(10) {
        println!("updated {name} {old} -> {new}");
    }
    for name in report.added.iter().take(10) {
        println!("added   {name}");
    }
    for name in report.removed.iter().take(10) {
        println!("removed {name}");
    }
    if report.added.len() > 10 {
        println!("... and {} more added", report.added.len() - 10);
    }
    if report.removed.len() > 10 {
        println!("... and {} more removed", report.removed.len() - 10);
    }
    println!("index written to {}", dest.display());
    Ok(())
}

fn cmd_packages(cmd: &Command) -> Vec<String> {
    match cmd {
        Command::Resolve { packages }
        | Command::Download { packages }
        | Command::Verify { packages } => packages.clone(),
        Command::Index(_)
        | Command::Sync { .. }
        | Command::Install { .. }
        | Command::Remove { .. }
        | Command::List { .. } => Vec::new(),
    }
}

async fn index_cmd(cmd: IndexCmd) -> Result<()> {
    match cmd {
        IndexCmd::Pack { input, output } => {
            let bytes = std::fs::read(&input)?;
            let index = Index::from_json(&bytes)?;
            let pkgs: Vec<_> = index.packages().values().cloned().collect();
            Index::write_binary(&output, &pkgs)?;
            eprintln!("packed {} packages into {}", pkgs.len(), output.display());
        }
        IndexCmd::Fetch { url, output } => {
            let bytes = download::fetch_bytes(&url).await?;
            let index = Index::from_json(&bytes)?;
            let pkgs: Vec<_> = index.packages().values().cloned().collect();
            Index::write_binary_with_mirrors(&output, &pkgs, index.mirrors())?;
            eprintln!(
                "fetched {} packages and {} mirror sets into {}",
                pkgs.len(),
                index.mirrors().len(),
                output.display()
            );
        }
        IndexCmd::Info { path } => {
            let index = Index::load(&path)?;
            let pkgs = index.packages();
            let provides: usize = pkgs.values().map(|p| p.provides.len()).sum();
            eprintln!("{} packages, {provides} provides", pkgs.len());
            for p in pkgs.values().take(20) {
                println!("{} {} ({} deps)", p.name, p.version, p.deps.len());
            }
        }
    }
    Ok(())
}

fn is_root() -> bool {
    unsafe { libc::geteuid() == 0 }
}

fn layout(root: Option<PathBuf>, bin: Option<PathBuf>, system: bool) -> install::Layout {
    if system || is_root() {
        let mut layout = install::Layout::system();
        if let Some(root) = root {
            layout.root = root;
        }
        if let Some(bin) = bin {
            layout.bin = bin;
        }
        return layout;
    }
    let mut layout = install::Layout::user();
    if let Some(root) = root {
        layout.root = root;
    }
    if let Some(bin) = bin {
        layout.bin = bin;
    }
    layout
}

async fn install_cmd(
    packages: Vec<String>,
    root: Option<PathBuf>,
    bin: Option<PathBuf>,
    index: Option<String>,
    concurrency: usize,
    yes: bool,
    system: bool,
) -> Result<()> {
    if packages.is_empty() {
        anyhow::bail!("nothing to install");
    }

    let layout = layout(root, bin, system);
    let t0 = Instant::now();
    let index = load_index(index.as_deref().unwrap_or_default()).await?;
    let index_mirrors = index.mirrors().to_vec();
    let repo: std::sync::Arc<dyn repo::Repository> = std::sync::Arc::new(index);
    eprintln!("loaded index in {}", format::duration(t0.elapsed()));

    let t1 = Instant::now();
    let res = resolver::resolve(&packages, std::sync::Arc::clone(&repo)).await?;
    eprintln!(
        "resolved {} packages in {}",
        res.packages.len(),
        format::duration(t1.elapsed())
    );

    let urls: Vec<String> = res.packages.iter().map(|p| p.url.clone()).collect();
    let sizes = download::remote_sizes(&urls).await;

    println!();
    println!("Install packages:");
    for (pkg, size) in res.packages.iter().zip(&sizes) {
        let size = match (pkg.size, size) {
            (0, Some(remote)) => format::size(*remote),
            (0, None) => "unknown size".to_string(),
            (known, _) => format::size(known),
        };
        println!("  - {} {} ({size})", pkg.name, pkg.version);
    }
    println!();

    if !yes && !confirm("Do you want to install these packages? (Y/n) ").await? {
        println!("nothing was installed");
        return Ok(());
    }

    let dl = download::Downloader::with_mirrors(cache_dir(), concurrency, index_mirrors)?;
    let t2 = Instant::now();
    let paths = dl.fetch_all(&res.packages).await?;
    let elapsed = t2.elapsed();
    let downloaded: u64 = paths
        .iter()
        .filter_map(|p| std::fs::metadata(p).ok())
        .map(|m| m.len())
        .sum();
    eprintln!(
        "downloaded {} packages in {} ({})",
        paths.len(),
        format::duration(elapsed),
        format::speed(downloaded, elapsed)
    );

    let unverified = res.packages.iter().filter(|p| p.sha256.is_empty()).count();
    if unverified > 0 {
        eprintln!("{unverified} packages have no checksum in the index and were not verified");
    }

    let t3 = Instant::now();
    for (pkg, archive) in res.packages.iter().zip(&paths) {
        println!("Installing {} {}...", pkg.name, pkg.version);
        let receipt = install::install(pkg, archive, &layout)?;
        println!("  installed {} {}", receipt.name, receipt.version);
        for (link, target) in &receipt.links {
            let _ = target;
            println!("  {}", link.display());
        }
        if receipt.links.is_empty() && receipt.conflicts.is_empty() {
            println!("  no binaries found in {}", archive.display());
        }
        for (link, owner) in &receipt.conflicts {
            println!("  {} is taken by {}, not linked", link.display(), owner);
        }
    }
    eprintln!(
        "installed {} packages in {}",
        res.packages.len(),
        format::duration(t3.elapsed())
    );
    Ok(())
}

async fn confirm(question: &str) -> Result<bool> {
    use tokio::io::AsyncBufReadExt;

    use std::io::Write;
    print!("{question}");
    std::io::stdout().flush()?;

    let mut lines = tokio::io::BufReader::new(tokio::io::stdin()).lines();
    let answer = lines.next_line().await?.unwrap_or_default();
    let answer = answer.trim().to_ascii_lowercase();
    Ok(answer.is_empty() || answer == "y" || answer == "yes")
}

fn remove_cmd(
    packages: Vec<String>,
    root: Option<PathBuf>,
    bin: Option<PathBuf>,
    system: bool,
) -> Result<()> {
    let layout = layout(root, bin, system);
    for name in &packages {
        let receipt = install::remove(name, &layout)?;
        println!("removed {} {}", receipt.name, receipt.version);
    }
    Ok(())
}

fn list_cmd(
    packages: Vec<String>,
    root: Option<PathBuf>,
    bin: Option<PathBuf>,
    system: bool,
) -> Result<()> {
    let layout = layout(root, bin, system);
    let receipts = install::installed(&layout);
    if receipts.is_empty() {
        if packages.is_empty() {
            println!("nothing installed");
        }
        return Ok(());
    }
    for receipt in receipts {
        if !packages.is_empty() && !packages.iter().any(|p| p == &receipt.name) {
            continue;
        }
        let description = receipt
            .description
            .as_deref()
            .map(|d| format!(" - {d}"))
            .unwrap_or_default();
        println!("{} {}{}", receipt.name, receipt.version, description);
    }
    Ok(())
}
