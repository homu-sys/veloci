use crate::repo::Package;
use anyhow::{bail, Context, Result};
use serde::{Deserialize, Serialize};
use std::io::Read;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Receipt {
    pub name: String,
    pub version: String,
    #[serde(default)]
    pub description: Option<String>,
    pub url: String,
    #[serde(default)]
    pub sha256: String,

    pub root: PathBuf,

    pub files: Vec<PathBuf>,

    pub links: Vec<(PathBuf, PathBuf)>,

    #[serde(default)]
    pub conflicts: Vec<(PathBuf, String)>,
}

#[derive(Clone, Debug)]
pub struct Layout {
    pub root: PathBuf,

    pub bin: PathBuf,

    pub receipts: PathBuf,
}

impl Layout {
    pub fn user() -> Self {
        let data = dirs::data_local_dir().unwrap_or_else(|| PathBuf::from(".veloci"));
        let bin = dirs::home_dir()
            .unwrap_or_else(|| PathBuf::from("."))
            .join(".local/bin");
        Self {
            root: data.join("veloci/packages"),
            bin,
            receipts: data.join("veloci/receipts"),
        }
    }

    pub fn package_dir(&self, name: &str) -> PathBuf {
        self.root.join(name)
    }

    pub fn receipt(&self, name: &str) -> PathBuf {
        self.receipts.join(format!("{name}.json"))
    }
}

pub fn unpack(archive: &Path, dest: &Path, strip_prefix: Option<&str>) -> Result<Vec<PathBuf>> {
    let file =
        std::fs::File::open(archive).with_context(|| format!("opening {}", archive.display()))?;

    let name = archive.file_name().unwrap_or_default().to_string_lossy();
    let mut written = Vec::new();

    if name.ends_with(".tar.gz") || name.ends_with(".tgz") {
        let reader = flate2::read::GzDecoder::new(file);
        unpack_tar(reader, dest, strip_prefix, &mut written)?;
    } else if name.ends_with(".tar.xz") || name.ends_with(".txz") {
        let mut out = Vec::new();
        let mut reader = std::io::BufReader::new(file);
        lzma_rs::xz_decompress(&mut reader, &mut out).context("decompressing the xz archive")?;
        unpack_tar(std::io::Cursor::new(out), dest, strip_prefix, &mut written)?;
    } else if name.ends_with(".tar.zst") || name.ends_with(".tzst") {
        let decoder =
            ruzstd::decoding::StreamingDecoder::new(file).context("reading the zstd archive")?;
        unpack_tar(decoder, dest, strip_prefix, &mut written)?;
    } else if name.ends_with(".tar.bz2") || name.ends_with(".tbz2") {
        bail!("bzip2 archives are not supported yet");
    } else if name.ends_with(".tar") {
        unpack_tar(file, dest, strip_prefix, &mut written)?;
    } else {
        bail!(
            "cannot unpack {}: only tar archives are supported",
            archive.display()
        );
    }

    Ok(written)
}

fn unpack_tar<R: Read>(
    reader: R,
    dest: &Path,
    strip_prefix: Option<&str>,
    written: &mut Vec<PathBuf>,
) -> Result<()> {
    let mut archive = tar::Archive::new(reader);
    let entries = archive.entries().context("reading the archive")?;

    for entry in entries {
        let mut entry = entry.context("reading an archive entry")?;
        let path = entry
            .path()
            .context("reading an archive path")?
            .into_owned();

        let relative = match strip_prefix {
            Some(prefix) => match strip(&path, prefix) {
                Some(rest) => rest,

                None => continue,
            },
            None => path.clone(),
        };

        if relative.as_os_str().is_empty() {
            continue;
        }

        if relative
            .components()
            .any(|c| matches!(c, std::path::Component::ParentDir))
        {
            continue;
        }

        let target = dest.join(&relative);
        if let Some(parent) = target.parent() {
            std::fs::create_dir_all(parent)
                .with_context(|| format!("creating {}", parent.display()))?;
        }

        match entry.header().entry_type() {
            tar::EntryType::Directory => {
                std::fs::create_dir_all(&target)
                    .with_context(|| format!("creating {}", target.display()))?;
            }
            tar::EntryType::Symlink => {
                if let Some(link) = entry.link_name().context("reading a link")? {
                    let link = link.into_owned();
                    let _ = std::fs::remove_file(&target);
                    std::os::unix::fs::symlink(&link, &target)
                        .with_context(|| format!("linking {}", target.display()))?;
                }
            }
            tar::EntryType::Link => {
                if let Some(link) = entry.link_name().context("reading a link")? {
                    let link = link.into_owned();
                    let source = match strip_prefix {
                        Some(prefix) => dest.join(strip(&link, prefix).unwrap_or(link.clone())),
                        None => dest.join(&link),
                    };
                    let _ = std::fs::hard_link(&source, &target);
                }
            }
            tar::EntryType::Regular | tar::EntryType::Continuous => {
                let mut out = std::fs::File::create(&target)
                    .with_context(|| format!("creating {}", target.display()))?;
                std::io::copy(&mut entry, &mut out)
                    .with_context(|| format!("writing {}", target.display()))?;
                let mode = entry.header().mode().unwrap_or(0o644);
                let _ = std::fs::set_permissions(
                    &target,
                    std::fs::Permissions::from_mode(mode & 0o7777),
                );
            }
            _ => {}
        }
        written.push(target);
    }
    Ok(())
}

fn strip(path: &Path, prefix: &str) -> Option<PathBuf> {
    let mut components = path.components();
    let first = components.next()?;
    if first.as_os_str() != prefix {
        return None;
    }
    let rest: PathBuf = components.collect();
    if rest.as_os_str().is_empty() {
        None
    } else {
        Some(rest)
    }
}

pub fn find_bins(root: &Path, wanted: &[String], package: &str) -> Vec<PathBuf> {
    let mut found = Vec::new();

    for name in wanted {
        let direct = root.join(name);
        if direct.is_file() {
            found.push(direct);
            continue;
        }
        if let Some(path) = search(root, name) {
            found.push(path);
        }
    }

    if !found.is_empty() {
        return found;
    }

    if !package.is_empty() {
        if let Some(path) = search(root, package) {
            return vec![path];
        }
    }

    let mut executables = top_level_executables(root);
    if executables.is_empty() {
        if let Some(dir) = only_directory(root) {
            executables = top_level_executables(&dir);
        }
    }
    executables
}

fn top_level_executables(root: &Path) -> Vec<PathBuf> {
    let Ok(entries) = std::fs::read_dir(root) else {
        return Vec::new();
    };
    let mut executables: Vec<PathBuf> = entries
        .filter_map(|e| e.ok())
        .map(|e| e.path())
        .filter(|p| p.is_file() && is_executable(p) && is_elf(p))
        .collect();
    executables.sort();
    executables
}

fn only_directory(root: &Path) -> Option<PathBuf> {
    let entries: Vec<PathBuf> = std::fs::read_dir(root)
        .ok()?
        .filter_map(|e| e.ok())
        .map(|e| e.path())
        .filter(|p| p.is_dir())
        .collect();
    match entries.len() {
        1 => Some(entries.into_iter().next().unwrap()),
        _ => None,
    }
}

fn is_elf(path: &Path) -> bool {
    use std::io::Read;
    let Ok(mut file) = std::fs::File::open(path) else {
        return false;
    };
    let mut magic = [0u8; 4];
    file.read_exact(&mut magic).is_ok() && &magic == b"\x7fELF"
}

fn is_program(path: &Path) -> bool {
    use std::io::{Read, Seek, SeekFrom};
    if is_elf(path) {
        return true;
    }
    let Ok(mut file) = std::fs::File::open(path) else {
        return false;
    };
    if file.seek(SeekFrom::Start(0)).is_err() {
        return false;
    }
    let mut start = [0u8; 2];
    file.read_exact(&mut start).is_ok() && &start == b"#!"
}

fn search(root: &Path, name: &str) -> Option<PathBuf> {
    let mut stack = vec![root.to_path_buf()];
    while let Some(dir) = stack.pop() {
        let Ok(entries) = std::fs::read_dir(&dir) else {
            continue;
        };
        for entry in entries.filter_map(|e| e.ok()) {
            let path = entry.path();
            if path.is_dir() {
                stack.push(path);
            } else if path.file_name().is_some_and(|n| n == name)
                && is_executable(&path)
                && is_program(&path)
            {
                return Some(path);
            }
        }
    }
    None
}

fn is_executable(path: &Path) -> bool {
    std::fs::metadata(path)
        .map(|m| m.permissions().mode() & 0o111 != 0)
        .unwrap_or(false)
}

pub fn install(pkg: &Package, archive: &Path, layout: &Layout) -> Result<Receipt> {
    let dest = layout.package_dir(&pkg.name);
    if dest.exists() {
        std::fs::remove_dir_all(&dest)
            .with_context(|| format!("removing the old {}", dest.display()))?;
    }
    std::fs::create_dir_all(&dest).with_context(|| format!("creating {}", dest.display()))?;

    let files = unpack(archive, &dest, pkg.strip_prefix.as_deref())?;

    let mut links = Vec::new();
    let mut conflicts = Vec::new();
    for bin in find_bins(&dest, &pkg.bins, &pkg.name) {
        let Some(name) = bin.file_name() else {
            continue;
        };
        let link = layout.bin.join(name);
        if let Some(parent) = link.parent() {
            std::fs::create_dir_all(parent)
                .with_context(|| format!("creating {}", parent.display()))?;
        }

        match std::fs::read_link(&link) {
            Ok(target) if target.starts_with(&dest) => {
                let _ = std::fs::remove_file(&link);
            }

            Ok(target) => {
                conflicts.push((link.clone(), target.display().to_string()));
                continue;
            }
            Err(_) => {
                if link.exists() {
                    conflicts.push((link.clone(), "a file that is already there".to_string()));
                    continue;
                }
            }
        }

        std::os::unix::fs::symlink(&bin, &link)
            .with_context(|| format!("linking {}", link.display()))?;
        links.push((link, bin));
    }

    let receipt = Receipt {
        name: pkg.name.clone(),
        version: pkg.version.clone(),
        description: pkg.description.clone(),
        url: pkg.url.clone(),
        sha256: pkg.sha256.clone(),
        root: dest,
        files,
        links,
        conflicts,
    };
    write_receipt(&layout.receipt(&pkg.name), &receipt)?;
    Ok(receipt)
}

pub fn write_receipt(path: &Path, receipt: &Receipt) -> Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)
            .with_context(|| format!("creating {}", parent.display()))?;
    }
    let text = serde_json::to_string_pretty(receipt).context("serializing the receipt")?;
    std::fs::write(path, text).with_context(|| format!("writing {}", path.display()))?;
    Ok(())
}

pub fn read_receipt(path: &Path) -> Result<Receipt> {
    let text =
        std::fs::read_to_string(path).with_context(|| format!("reading {}", path.display()))?;
    serde_json::from_str(&text).with_context(|| format!("parsing {}", path.display()))
}

pub fn remove(name: &str, layout: &Layout) -> Result<Receipt> {
    let path = layout.receipt(name);
    if !path.exists() {
        bail!("{name} is not installed");
    }
    let receipt = read_receipt(&path)?;

    for (link, target) in &receipt.links {
        if std::fs::read_link(link).is_ok_and(|t| t == *target) {
            let _ = std::fs::remove_file(link);
        }
    }
    for link in links_into(&layout.bin, &receipt.root) {
        let _ = std::fs::remove_file(&link);
    }
    if receipt.root.exists() {
        std::fs::remove_dir_all(&receipt.root)
            .with_context(|| format!("removing {}", receipt.root.display()))?;
    }
    std::fs::remove_file(&path).with_context(|| format!("removing {}", path.display()))?;
    Ok(receipt)
}

fn links_into(bin: &Path, root: &Path) -> Vec<PathBuf> {
    let Ok(entries) = std::fs::read_dir(bin) else {
        return Vec::new();
    };
    entries
        .filter_map(|e| e.ok())
        .map(|e| e.path())
        .filter(|p| std::fs::read_link(p).is_ok_and(|t| t.starts_with(root)))
        .collect()
}

pub fn installed(layout: &Layout) -> Vec<Receipt> {
    let Ok(entries) = std::fs::read_dir(&layout.receipts) else {
        return Vec::new();
    };
    let mut receipts: Vec<Receipt> = entries
        .filter_map(|e| e.ok())
        .filter(|e| e.path().extension().is_some_and(|x| x == "json"))
        .filter_map(|e| read_receipt(&e.path()).ok())
        .collect();
    receipts.sort_by(|a, b| a.name.cmp(&b.name));
    receipts
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn strips_a_leading_directory() {
        assert_eq!(
            strip(Path::new("bat-0.26.1/bat"), "bat-0.26.1"),
            Some(PathBuf::from("bat"))
        );
        assert_eq!(
            strip(Path::new("bat-0.26.1/autocomplete/bat.fish"), "bat-0.26.1"),
            Some(PathBuf::from("autocomplete/bat.fish"))
        );
        assert_eq!(strip(Path::new("other/bat"), "bat-0.26.1"), None);
        assert_eq!(strip(Path::new("bat-0.26.1"), "bat-0.26.1"), None);
    }
}

#[cfg(test)]
mod archive_tests {
    use super::*;

    fn archive(dir: &Path, prefix: &str) -> PathBuf {
        let path = dir.join("package.tar.gz");
        let file = std::fs::File::create(&path).unwrap();
        let encoder = flate2::write::GzEncoder::new(file, flate2::Compression::fast());
        let mut builder = tar::Builder::new(encoder);

        let mut header = tar::Header::new_gnu();
        header.set_size(18);
        header.set_mode(0o755);
        header.set_cksum();
        builder
            .append_data(
                &mut header,
                format!("{prefix}/tool"),
                &b"#!/bin/sh\nexit 0\n\n"[..],
            )
            .unwrap();

        let mut header = tar::Header::new_gnu();
        header.set_size(3);
        header.set_mode(0o644);
        header.set_cksum();
        builder
            .append_data(&mut header, format!("{prefix}/README"), &b"doc"[..])
            .unwrap();

        builder.into_inner().unwrap().finish().unwrap();
        path
    }

    #[test]
    fn unpacks_an_archive_and_strips_the_prefix() {
        let tmp = tempfile::tempdir().unwrap();
        let path = archive(tmp.path(), "tool-1.0");
        let dest = tmp.path().join("unpacked");

        let files = unpack(&path, &dest, Some("tool-1.0")).unwrap();

        assert!(dest.join("tool").exists());
        assert!(dest.join("README").exists());
        assert!(!dest.join("tool-1.0").exists());
        assert_eq!(files.len(), 2);

        assert!(is_executable(&dest.join("tool")));
        assert!(!is_executable(&dest.join("README")));
    }

    #[test]
    fn finds_the_binaries_of_an_unpacked_package() {
        let tmp = tempfile::tempdir().unwrap();
        let path = archive(tmp.path(), "tool-1.0");
        let dest = tmp.path().join("unpacked");
        unpack(&path, &dest, Some("tool-1.0")).unwrap();

        let bins = find_bins(&dest, &["tool".to_string()], "tool");
        assert_eq!(bins, vec![dest.join("tool")]);

        let bins = find_bins(&dest, &[], "tool");
        assert_eq!(bins, vec![dest.join("tool")]);
    }

    #[test]
    fn writes_and_removes_a_package() {
        let tmp = tempfile::tempdir().unwrap();
        let path = archive(tmp.path(), "tool-1.0");
        let dest = tmp.path().join("unpacked");
        unpack(&path, &dest, Some("tool-1.0")).unwrap();

        let layout = Layout {
            root: tmp.path().join("packages"),
            bin: tmp.path().join("bin"),
            receipts: tmp.path().join("receipts"),
        };
        let pkg = Package {
            name: "tool".to_string(),
            version: "1.0".to_string(),
            url: "https://example.org/tool.tar.gz".to_string(),
            bins: vec!["tool".to_string()],
            ..Package::default()
        };

        let receipt = install(&pkg, &path, &layout).unwrap();
        assert!(layout.bin.join("tool").exists());
        assert!(layout.receipt("tool").exists());
        assert_eq!(receipt.links.len(), 1);

        let receipt = install(&pkg, &path, &layout).unwrap();
        assert_eq!(receipt.links.len(), 1);

        let receipts = installed(&layout);
        assert_eq!(receipts.len(), 1);
        assert_eq!(receipts[0].name, "tool");

        remove("tool", &layout).unwrap();
        assert!(!layout.bin.join("tool").exists());
        assert!(!layout.package_dir("tool").exists());
        assert!(installed(&layout).is_empty());
    }

    #[test]
    fn leaves_a_name_that_belongs_to_another_package_alone() {
        let tmp = tempfile::tempdir().unwrap();
        let path = archive(tmp.path(), "tool-1.0");

        let layout = Layout {
            root: tmp.path().join("packages"),
            bin: tmp.path().join("bin"),
            receipts: tmp.path().join("receipts"),
        };
        std::fs::create_dir_all(&layout.bin).unwrap();
        let other = tmp.path().join("other");
        std::fs::create_dir_all(&other).unwrap();
        let other_tool = other.join("tool");
        std::fs::write(&other_tool, b"#!/bin/sh\n").unwrap();
        std::os::unix::fs::symlink(&other_tool, layout.bin.join("tool")).unwrap();

        let pkg = Package {
            name: "tool".to_string(),
            version: "1.0".to_string(),
            url: "https://example.org/tool.tar.gz".to_string(),
            bins: vec!["tool".to_string()],
            ..Package::default()
        };

        let receipt = install(&pkg, &path, &layout).unwrap();
        assert!(receipt.links.is_empty());
        assert_eq!(receipt.conflicts.len(), 1);
        assert_eq!(
            std::fs::read_link(layout.bin.join("tool")).unwrap(),
            other_tool
        );
    }

    #[test]
    fn takes_its_own_name_back() {
        let tmp = tempfile::tempdir().unwrap();
        let path = archive(tmp.path(), "tool-1.0");

        let layout = Layout {
            root: tmp.path().join("packages"),
            bin: tmp.path().join("bin"),
            receipts: tmp.path().join("receipts"),
        };
        std::fs::create_dir_all(&layout.bin).unwrap();
        let mine = tmp.path().join("packages").join("tool").join("tool");
        std::fs::create_dir_all(mine.parent().unwrap()).unwrap();
        std::fs::write(&mine, b"#!/bin/sh\n").unwrap();
        std::os::unix::fs::symlink(&mine, layout.bin.join("tool")).unwrap();

        let pkg = Package {
            name: "tool".to_string(),
            version: "1.0".to_string(),
            url: "https://example.org/tool.tar.gz".to_string(),
            bins: vec!["tool".to_string()],
            ..Package::default()
        };

        let receipt = install(&pkg, &path, &layout).unwrap();
        assert_eq!(receipt.links.len(), 1);
        assert!(receipt.conflicts.is_empty());
    }
}
