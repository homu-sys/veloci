use crate::repo::{Package, PkgName, Provider, Repository};
use anyhow::{bail, Context, Result};
use std::collections::HashMap;
use std::io::Write;
use std::path::Path;

const MAGIC: &[u8] = b"VELOCIIDX\x03";

const OLD_MAGIC: &[u8] = b"VELOCIIDX\x02";

const NO_MIRROR: u32 = u32::MAX;

pub struct Index {
    packages: HashMap<PkgName, Package>,
    provides: HashMap<PkgName, Vec<Provider>>,

    mirrors: Vec<Vec<String>>,
}

impl Index {
    pub fn from_packages(pkgs: impl IntoIterator<Item = Package>) -> Self {
        Self::with_mirrors(pkgs, Vec::new())
    }

    pub fn with_mirrors(
        pkgs: impl IntoIterator<Item = Package>,
        mirrors: Vec<Vec<String>>,
    ) -> Self {
        let mut packages: HashMap<PkgName, Package> = HashMap::new();

        for pkg in pkgs {
            match packages.get(&pkg.name) {
                Some(existing)
                    if crate::version::vercmp(&pkg.version, &existing.version)
                        != std::cmp::Ordering::Greater =>
                {
                    let mut merged = existing.clone();
                    for p in &pkg.provides {
                        if !merged.provides.contains(p) {
                            merged.provides.push(p.clone());
                        }
                    }
                    packages.insert(merged.name.clone(), merged);
                }
                _ => {
                    packages.insert(pkg.name.clone(), pkg);
                }
            }
        }

        let mut provides: HashMap<PkgName, Vec<Provider>> = HashMap::new();
        for pkg in packages.values() {
            for p in &pkg.provides {
                let c = crate::constraint::Constraint::parse(p);
                provides.entry(c.name).or_default().push(Provider {
                    pkg: pkg.name.clone(),
                    version: c.version,
                });
            }
        }
        for list in provides.values_mut() {
            list.sort_by(|a, b| {
                let va = packages.get(&a.pkg).map(|p| &p.version);
                let vb = packages.get(&b.pkg).map(|p| &p.version);
                match (va, vb) {
                    (Some(a), Some(b)) => crate::version::vercmp(b, a),
                    _ => std::cmp::Ordering::Equal,
                }
            });
        }

        Self {
            packages,
            provides,
            mirrors,
        }
    }

    pub fn mirrors(&self) -> &[Vec<String>] {
        &self.mirrors
    }

    pub fn load(path: &Path) -> Result<Self> {
        let bytes =
            std::fs::read(path).with_context(|| format!("reading index {}", path.display()))?;
        if bytes.starts_with(MAGIC) {
            Self::from_binary(&bytes)
        } else if bytes.starts_with(OLD_MAGIC) {
            bail!(
                "the index {} is in an old format. Run `velo sync` to write a new one",
                path.display()
            )
        } else {
            Self::from_json(&bytes)
        }
    }

    pub fn from_json(bytes: &[u8]) -> Result<Self> {
        let raw: serde_json::Value = serde_json::from_slice(bytes)?;
        let list = match &raw {
            serde_json::Value::Array(items) => items.clone(),
            serde_json::Value::Object(_) => raw
                .get("packages")
                .and_then(|v| v.as_array())
                .cloned()
                .unwrap_or_default(),
            _ => Vec::new(),
        };
        let mirrors: Vec<Vec<String>> = raw
            .get("mirrors")
            .and_then(|v| v.as_array())
            .map(|sets| {
                sets.iter()
                    .map(|s| {
                        s.as_array()
                            .map(|l| {
                                l.iter()
                                    .filter_map(|v| v.as_str().map(str::to_string))
                                    .collect()
                            })
                            .unwrap_or_default()
                    })
                    .collect()
            })
            .unwrap_or_default();

        let mut pkgs = Vec::with_capacity(list.len());
        for v in list {
            pkgs.push(package_from_json(v)?);
        }
        Ok(Self::with_mirrors(pkgs, mirrors))
    }

    pub fn from_binary(bytes: &[u8]) -> Result<Self> {
        let mut cur = Cursor {
            bytes,
            pos: MAGIC.len(),
        };
        let mirror_count = cur.u32()? as usize;
        let mut mirrors = Vec::with_capacity(mirror_count);
        for _ in 0..mirror_count {
            let count = cur.u32()? as usize;
            let mut set = Vec::with_capacity(count);
            for _ in 0..count {
                set.push(cur.string()?);
            }
            mirrors.push(set);
        }
        let count = cur.u32()? as usize;
        let mut pkgs = Vec::with_capacity(count);
        for _ in 0..count {
            let name = cur.string()?;
            let version = cur.string()?;
            let deps = cur.string_list()?;
            let provides = cur.string_list()?;
            let conflicts = cur.string_list()?;
            let url = cur.string()?;
            let size = cur.u64()?;
            let sha256 = cur.string()?;
            let mirror = cur.u32()?;
            let mirror = if mirror == NO_MIRROR {
                None
            } else {
                Some(mirror)
            };
            let description = cur.string()?;
            let strip_prefix = cur.string()?;
            let strip_prefix = if strip_prefix.is_empty() {
                None
            } else {
                Some(strip_prefix)
            };
            let bins = cur.string_list()?;
            pkgs.push(Package {
                name,
                version,
                deps,
                provides,
                conflicts,
                url,
                size,
                sha256,
                mirror,
                bins,
                strip_prefix,
                description: if description.is_empty() {
                    None
                } else {
                    Some(description)
                },
            });
        }
        Ok(Self::with_mirrors(pkgs, mirrors))
    }

    pub fn write_binary(path: &Path, pkgs: &[Package]) -> Result<()> {
        Self::write_binary_with_mirrors(path, pkgs, &[])
    }

    pub fn write_binary_with_mirrors(
        path: &Path,
        pkgs: &[Package],
        mirrors: &[Vec<String>],
    ) -> Result<()> {
        let mut out: Vec<u8> = Vec::with_capacity(pkgs.len() * 128);
        out.extend_from_slice(MAGIC);
        out.extend_from_slice(&(mirrors.len() as u32).to_le_bytes());
        for set in mirrors {
            out.extend_from_slice(&(set.len() as u32).to_le_bytes());
            for base in set {
                put_string(&mut out, base);
            }
        }
        out.extend_from_slice(&(pkgs.len() as u32).to_le_bytes());
        for p in pkgs {
            put_string(&mut out, &p.name);
            put_string(&mut out, &p.version);
            put_string_list(&mut out, &p.deps);
            put_string_list(&mut out, &p.provides);
            put_string_list(&mut out, &p.conflicts);
            put_string(&mut out, &p.url);
            out.extend_from_slice(&p.size.to_le_bytes());
            put_string(&mut out, &p.sha256);
            out.extend_from_slice(&p.mirror.unwrap_or(NO_MIRROR).to_le_bytes());
            put_string(&mut out, p.description.as_deref().unwrap_or(""));
            put_string(&mut out, p.strip_prefix.as_deref().unwrap_or(""));
            put_string_list(&mut out, &p.bins);
        }

        let file = std::fs::File::create(path)
            .with_context(|| format!("writing index {}", path.display()))?;
        let mut file = std::io::BufWriter::new(file);
        file.write_all(&out)?;
        file.flush()?;
        Ok(())
    }

    pub fn packages(&self) -> &HashMap<PkgName, Package> {
        &self.packages
    }
}

impl Repository for Index {
    fn candidates(&self) -> &HashMap<PkgName, Package> {
        &self.packages
    }

    fn providers(&self, name: &str) -> Vec<Provider> {
        self.provides.get(name).cloned().unwrap_or_default()
    }
}

fn package_from_json(value: serde_json::Value) -> Result<Package> {
    let mut object = match value {
        serde_json::Value::Object(map) => map,
        other => bail!("package entry is not an object: {other}"),
    };

    if let Some(serde_json::Value::Object(map)) = object.remove("package") {
        for (k, v) in map {
            object.entry(k).or_insert(v);
        }
    }
    if let Some(serde_json::Value::Object(map)) = object.remove("archive") {
        for (k, v) in map {
            object.entry(k).or_insert(v);
        }
    }

    let get = |names: &[&str]| -> Option<&serde_json::Value> {
        names
            .iter()
            .find_map(|n| object.get(*n).filter(|v| !v.is_null()))
    };

    let name = get(&["name", "pkgname", "package"])
        .and_then(|v| v.as_str())
        .ok_or_else(|| anyhow::anyhow!("package entry has no name"))?
        .to_string();
    let version = get(&["version", "pkgver"])
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();

    let list = |names: &[&str]| -> Vec<String> {
        match get(names) {
            Some(value) => match value {
                serde_json::Value::Object(map) => map
                    .get("depends")
                    .or_else(|| map.get("requires"))
                    .or_else(|| map.get("deps"))
                    .map(strings)
                    .unwrap_or_default(),
                other => strings(other),
            },
            None => Vec::new(),
        }
    };

    let url = get(&["url", "download_url", "download", "filename", "file"])
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();
    let size = get(&["size", "csize", "download_size"])
        .and_then(|v| v.as_u64())
        .unwrap_or(0);
    let sha256 = get(&["sha256", "sha256sum", "checksum", "sha256_sum"])
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();
    let mirror = get(&["mirror"]).and_then(|v| v.as_u64()).map(|v| v as u32);

    let install = object.get("install").and_then(|v| v.as_object());
    let bins = match install
        .and_then(|m| m.get("bins"))
        .or_else(|| install.and_then(|m| m.get("bin")))
    {
        Some(value) => strings(value),
        None => list(&["bins", "bin"]),
    };

    let strip_prefix = get(&["strip_prefix", "stripPrefix"])
        .and_then(|v| v.as_str())
        .filter(|s| !s.is_empty())
        .map(str::to_string);
    let description = get(&["description", "summary"])
        .and_then(|v| v.as_str())
        .map(str::to_string);

    Ok(Package {
        name,
        version,
        deps: list(&["deps", "dependencies", "requires", "depends"]),
        provides: list(&["provides", "provides_names"]),
        conflicts: list(&["conflicts", "replaces"]),
        url,
        size,
        sha256,
        mirror,
        bins,
        strip_prefix,
        description,
    })
}

fn strings(value: &serde_json::Value) -> Vec<String> {
    match value {
        serde_json::Value::Array(items) => items
            .iter()
            .filter_map(|v| v.as_str().map(str::to_string))
            .collect(),
        serde_json::Value::String(s) => s.split_whitespace().map(str::to_string).collect(),
        _ => Vec::new(),
    }
}

fn put_string(out: &mut Vec<u8>, s: &str) {
    out.extend_from_slice(&(s.len() as u32).to_le_bytes());
    out.extend_from_slice(s.as_bytes());
}

fn put_string_list(out: &mut Vec<u8>, list: &[String]) {
    out.extend_from_slice(&(list.len() as u32).to_le_bytes());
    for s in list {
        put_string(out, s);
    }
}

struct Cursor<'a> {
    bytes: &'a [u8],
    pos: usize,
}

impl<'a> Cursor<'a> {
    fn take(&mut self, n: usize) -> Result<&'a [u8]> {
        let end = self
            .pos
            .checked_add(n)
            .ok_or_else(|| anyhow::anyhow!("index truncated"))?;
        if end > self.bytes.len() {
            bail!("index truncated at byte {}", self.pos);
        }
        let s = &self.bytes[self.pos..end];
        self.pos = end;
        Ok(s)
    }

    fn u32(&mut self) -> Result<u32> {
        let b = self.take(4)?;
        Ok(u32::from_le_bytes([b[0], b[1], b[2], b[3]]))
    }

    fn u64(&mut self) -> Result<u64> {
        let b = self.take(8)?;
        Ok(u64::from_le_bytes([
            b[0], b[1], b[2], b[3], b[4], b[5], b[6], b[7],
        ]))
    }

    fn string(&mut self) -> Result<String> {
        let len = self.u32()? as usize;
        let b = self.take(len)?;
        String::from_utf8(b.to_vec()).context("index contains invalid UTF-8")
    }

    fn string_list(&mut self) -> Result<Vec<String>> {
        let count = self.u32()? as usize;
        let mut out = Vec::with_capacity(count);
        for _ in 0..count {
            out.push(self.string()?);
        }
        Ok(out)
    }
}
