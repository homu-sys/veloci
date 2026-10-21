use crate::config::Config;
use crate::index::Index;
use crate::repo::Package;
use anyhow::{Context, Result};
use std::collections::HashMap;
use std::path::Path;

#[derive(Clone, Debug, Default)]
pub struct Report {
    pub added: Vec<String>,

    pub removed: Vec<String>,

    pub updated: Vec<(String, String, String)>,

    pub unchanged: usize,

    pub total: usize,
}

pub async fn fetch(config: &Config) -> Result<(Vec<Package>, Vec<Vec<String>>)> {
    let mut packages = Vec::new();
    let mut mirrors: Vec<Vec<String>> = Vec::new();

    for repo in &config.repo {
        let bytes = crate::download::fetch_bytes(&repo.url)
            .await
            .with_context(|| format!("syncing {} from {}", repo.name, repo.url))?;
        let index = Index::from_json(&bytes)
            .with_context(|| format!("reading the index of {}", repo.name))?;

        let mut remap: HashMap<u32, u32> = HashMap::new();
        for (id, set) in index.mirrors().iter().enumerate() {
            remap.insert(id as u32, mirrors.len() as u32);
            mirrors.push(set.clone());
        }
        for pkg in index.packages().values() {
            let mut pkg = pkg.clone();
            if let Some(mirror) = pkg.mirror {
                if let Some(new) = remap.get(&mirror) {
                    pkg.mirror = Some(*new);
                }
            }
            packages.push(pkg);
        }
    }
    Ok((packages, mirrors))
}

pub fn compare(old: &[Package], new: &[Package]) -> Report {
    let old_versions: HashMap<&str, &str> = old
        .iter()
        .map(|p| (p.name.as_str(), p.version.as_str()))
        .collect();
    let new_versions: HashMap<&str, &str> = new
        .iter()
        .map(|p| (p.name.as_str(), p.version.as_str()))
        .collect();

    let mut report = Report {
        total: new.len(),
        ..Report::default()
    };

    for (name, version) in &new_versions {
        match old_versions.get(name) {
            None => report.added.push((*name).to_string()),
            Some(old_version) if old_version != version => {
                report.updated.push((
                    (*name).to_string(),
                    (*old_version).to_string(),
                    (*version).to_string(),
                ));
            }
            Some(_) => report.unchanged += 1,
        }
    }
    for name in old_versions.keys() {
        if !new_versions.contains_key(name) {
            report.removed.push((*name).to_string());
        }
    }

    report.added.sort();
    report.removed.sort();
    report.updated.sort();
    report
}

pub async fn sync(config: &Config, dest: &Path) -> Result<Report> {
    let (packages, mirrors) = fetch(config).await?;
    let index = Index::with_mirrors(packages, mirrors);
    let pkgs: Vec<Package> = index.packages().values().cloned().collect();

    let previous = if dest.exists() {
        Index::load(dest)
            .map(|i| i.packages().values().cloned().collect::<Vec<_>>())
            .unwrap_or_default()
    } else {
        Vec::new()
    };

    let report = compare(&previous, &pkgs);

    if let Some(parent) = dest.parent() {
        std::fs::create_dir_all(parent)
            .with_context(|| format!("creating {}", parent.display()))?;
    }
    Index::write_binary_with_mirrors(dest, &pkgs, index.mirrors())
        .with_context(|| format!("writing {}", dest.display()))?;

    Ok(report)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::repo::Package;

    fn pkg(name: &str, version: &str) -> Package {
        Package {
            name: name.to_string(),
            version: version.to_string(),
            ..Package::default()
        }
    }

    #[test]
    fn reports_what_changed() {
        let old = vec![pkg("a", "1"), pkg("b", "1"), pkg("c", "1")];
        let new = vec![pkg("a", "2"), pkg("b", "1"), pkg("d", "1")];

        let report = compare(&old, &new);
        assert_eq!(report.added, vec!["d".to_string()]);
        assert_eq!(report.removed, vec!["c".to_string()]);
        assert_eq!(
            report.updated,
            vec![("a".to_string(), "1".to_string(), "2".to_string())]
        );
        assert_eq!(report.unchanged, 1);
        assert_eq!(report.total, 3);
    }

    #[test]
    fn a_second_sync_changes_nothing() {
        let packages = vec![pkg("a", "1"), pkg("b", "2")];
        let report = compare(&packages, &packages);
        assert!(report.added.is_empty());
        assert!(report.removed.is_empty());
        assert!(report.updated.is_empty());
        assert_eq!(report.unchanged, 2);
    }
}
