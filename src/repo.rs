use serde::{Deserialize, Serialize};
use std::collections::HashMap;

pub type PkgName = String;

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct Package {
    pub name: PkgName,
    pub version: String,
    pub deps: Vec<PkgName>,
    pub provides: Vec<PkgName>,
    pub conflicts: Vec<PkgName>,
    pub url: String,
    pub size: u64,
    pub sha256: String,

    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mirror: Option<u32>,

    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub bins: Vec<String>,

    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub strip_prefix: Option<String>,

    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
}

#[derive(Clone, Debug)]
pub struct Provider {
    pub pkg: PkgName,

    pub version: Option<String>,
}

pub trait Repository: Send + Sync {
    fn candidates(&self) -> &HashMap<PkgName, Package>;

    fn providers(&self, name: &str) -> Vec<Provider>;
}

#[derive(Clone, Default)]
pub struct MemoryRepo {
    pub packages: HashMap<PkgName, Package>,
    pub provides: HashMap<PkgName, Vec<Provider>>,
}

impl MemoryRepo {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn add(&mut self, pkg: Package) {
        for p in &pkg.provides {
            let c = crate::constraint::Constraint::parse(p);
            self.provides.entry(c.name).or_default().push(Provider {
                pkg: pkg.name.clone(),
                version: c.version,
            });
        }
        self.packages.insert(pkg.name.clone(), pkg);
    }
}

impl Repository for MemoryRepo {
    fn candidates(&self) -> &HashMap<PkgName, Package> {
        &self.packages
    }

    fn providers(&self, name: &str) -> Vec<Provider> {
        self.provides.get(name).cloned().unwrap_or_default()
    }
}
