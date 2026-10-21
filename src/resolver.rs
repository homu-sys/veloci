use crate::repo::{Package, PkgName, Provider, Repository};
use anyhow::Result;
use dashmap::DashMap;
use std::cmp::Reverse;
use std::collections::{BinaryHeap, HashMap, HashSet, VecDeque};
use std::sync::Arc;

#[derive(Debug, Default)]
pub struct Resolution {
    pub order: Vec<PkgName>,
    pub packages: Vec<Package>,

    pub provider_of: HashMap<PkgName, PkgName>,

    pub cycles: Vec<Vec<PkgName>>,
}

impl Resolution {
    pub fn verify(&self) -> Vec<(PkgName, PkgName)> {
        let pos: HashMap<&PkgName, usize> =
            self.order.iter().enumerate().map(|(i, n)| (n, i)).collect();
        let mut bad = Vec::new();
        for p in &self.packages {
            let Some(&i) = pos.get(&p.name) else {
                continue;
            };
            for d in &p.deps {
                let Some(target) = self.provider_of.get(d) else {
                    continue;
                };
                let Some(&j) = pos.get(target) else {
                    continue;
                };
                if j > i && !self.in_cycle(&p.name, target) {
                    bad.push((p.name.clone(), target.clone()));
                }
            }
        }
        bad
    }

    fn in_cycle(&self, from: &str, to: &str) -> bool {
        self.cycles
            .iter()
            .any(|cycle| cycle.iter().any(|n| n == from) && cycle.iter().any(|n| n == to))
    }
}

pub async fn resolve(roots: &[PkgName], repo: Arc<dyn Repository>) -> Result<Resolution> {
    let seen: Arc<DashMap<PkgName, Package>> = Arc::new(DashMap::new());

    let mut satisfied: HashSet<PkgName> = HashSet::new();

    let mut provider_of: HashMap<PkgName, PkgName> = HashMap::new();

    let mut queue: VecDeque<PkgName> = roots.iter().cloned().collect();
    let mut queued: HashSet<PkgName> = queue.iter().cloned().collect();

    while !queue.is_empty() {
        let mut wave: Vec<PkgName> = Vec::new();
        while let Some(name) = queue.pop_front() {
            if satisfied.contains(&name) {
                continue;
            }
            wave.push(name);
        }
        if wave.is_empty() {
            break;
        }

        let mut handles = Vec::with_capacity(wave.len());
        for name in &wave {
            let name = name.clone();
            let repo = Arc::clone(&repo);
            let seen = Arc::clone(&seen);
            handles.push(tokio::spawn(async move {
                match pick(&name, repo.as_ref()) {
                    Some((pkg, names)) => {
                        let selected = pkg.name.clone();
                        seen.insert(pkg.name.clone(), pkg);
                        Ok::<_, anyhow::Error>((name, selected, names))
                    }
                    None => Err(anyhow::anyhow!("package not found: {name}")),
                }
            }));
        }

        let mut resolved_here: Vec<Package> = Vec::with_capacity(handles.len());
        for h in handles {
            let (_name, selected, names) = h.await??;

            for n in names {
                record_provider(&mut provider_of, &seen, &n, &selected);
                satisfied.insert(n);
            }

            if let Some(p) = seen.get(&selected) {
                resolved_here.push(p.clone());
            }
        }

        for pkg in &resolved_here {
            satisfied.insert(pkg.name.clone());
        }

        for pkg in &resolved_here {
            for d in &pkg.deps {
                if satisfied.contains(d) || queued.contains(d) {
                    continue;
                }
                queued.insert(d.clone());
                queue.push_back(d.clone());
            }
        }
    }

    let packages: Vec<Package> = seen.iter().map(|e| e.value().clone()).collect();
    let (order, cycles) = topo_sort(&packages, &provider_of);

    Ok(Resolution {
        order,
        packages,
        provider_of,
        cycles,
    })
}

fn record_provider(
    provider_of: &mut HashMap<PkgName, PkgName>,
    seen: &DashMap<PkgName, Package>,
    name: &PkgName,
    provider: &PkgName,
) {
    if let Some(current) = provider_of.get(name) {
        if current == provider {
            return;
        }
        let current_version = seen.get(current).map(|p| p.version.clone());
        let new_version = seen.get(provider).map(|p| p.version.clone());
        if let (Some(c), Some(n)) = (current_version, new_version) {
            if crate::version::vercmp(&n, &c) != std::cmp::Ordering::Greater {
                return;
            }
        }
    }
    provider_of.insert(name.clone(), provider.clone());
}

fn pick(dep: &str, repo: &dyn Repository) -> Option<(Package, Vec<PkgName>)> {
    let constraint = crate::constraint::Constraint::parse(dep);

    if let Some(p) = repo.candidates().get(&constraint.name) {
        if constraint.matches_version(&p.version) {
            let mut names = vec![p.name.clone()];
            names.extend(p.provides.iter().cloned());
            return Some((p.clone(), names));
        }
    }

    let mut best: Option<(Provider, &Package)> = None;
    for provider in repo.providers(&constraint.name) {
        if !constraint.matches_provided(provider.version.as_deref()) {
            continue;
        }
        let Some(pkg) = repo.candidates().get(&provider.pkg) else {
            continue;
        };
        best = match best {
            None => Some((provider, pkg)),
            Some((_, current)) => {
                if crate::version::vercmp(&pkg.version, &current.version)
                    == std::cmp::Ordering::Greater
                {
                    Some((provider, pkg))
                } else {
                    Some((provider, current))
                }
            }
        };
    }

    let (_, pkg) = best?;
    let mut names = vec![pkg.name.clone()];
    names.extend(pkg.provides.iter().cloned());
    names.push(dep.to_string());
    Some((pkg.clone(), names))
}

fn find_cycle(
    indegree: &HashMap<PkgName, usize>,
    provider_of: &HashMap<PkgName, PkgName>,
    by_name: &HashMap<&PkgName, &Package>,
    emitted: &HashSet<PkgName>,
) -> Option<Vec<PkgName>> {
    let mut starts: Vec<&PkgName> = indegree
        .iter()
        .filter(|(n, d)| **d > 0 && !emitted.contains(*n))
        .map(|(n, _)| n)
        .collect();
    starts.sort();

    let mut path: Vec<PkgName> = Vec::new();
    let mut on_path: HashMap<PkgName, usize> = HashMap::new();
    let mut visited: HashSet<PkgName> = HashSet::new();

    let mut stack: Vec<(PkgName, usize)> = Vec::new();

    for start in starts {
        if visited.contains(start) {
            continue;
        }
        visited.insert(start.clone());
        path.push(start.clone());
        on_path.insert(start.clone(), 0);
        stack.push((start.clone(), 0));

        while let Some(top) = stack.last() {
            let node = top.0.clone();
            let Some(pkg) = by_name.get(&node) else {
                stack.pop();
                on_path.remove(&node);
                path.pop();
                continue;
            };

            let mut descended = false;
            while stack.last().map(|t| t.1).unwrap_or(0) < pkg.deps.len() {
                let top = stack.len() - 1;
                let dep = pkg.deps[stack[top].1].clone();
                stack[top].1 += 1;

                let Some(target) = provider_of.get(&dep) else {
                    continue;
                };
                if target == &node || emitted.contains(target) || !by_name.contains_key(target) {
                    continue;
                }
                if let Some(&i) = on_path.get(target) {
                    return Some(path[i..].to_vec());
                }
                if visited.contains(target) {
                    continue;
                }
                visited.insert(target.clone());
                path.push(target.clone());
                on_path.insert(target.clone(), path.len() - 1);
                stack.push((target.clone(), 0));
                descended = true;
                break;
            }

            if !descended {
                stack.pop();
                on_path.remove(&node);
                path.pop();
            }
        }
    }
    None
}

fn topo_sort(
    pkgs: &[Package],
    provider_of: &HashMap<PkgName, PkgName>,
) -> (Vec<PkgName>, Vec<Vec<PkgName>>) {
    let by_name: HashMap<&PkgName, &Package> = pkgs.iter().map(|p| (&p.name, p)).collect();
    let mut indegree: HashMap<PkgName, usize> = HashMap::new();

    let mut dependents: HashMap<PkgName, Vec<PkgName>> = HashMap::new();

    for p in pkgs {
        indegree.entry(p.name.clone()).or_insert(0);
        for d in &p.deps {
            let Some(target) = provider_of.get(d) else {
                continue;
            };
            if !by_name.contains_key(target) {
                continue;
            }
            if target == &p.name {
                continue;
            }
            *indegree.entry(p.name.clone()).or_insert(0) += 1;
            dependents
                .entry(target.clone())
                .or_default()
                .push(p.name.clone());
        }
    }

    let mut ready: BinaryHeap<Reverse<PkgName>> = indegree
        .iter()
        .filter(|(_, &d)| d == 0)
        .map(|(n, _)| Reverse(n.clone()))
        .collect();

    let mut order: Vec<PkgName> = Vec::with_capacity(pkgs.len());
    let mut emitted: HashSet<PkgName> = HashSet::new();
    let mut cycles: Vec<Vec<PkgName>> = Vec::new();

    while order.len() < pkgs.len() {
        let n = if let Some(Reverse(n)) = ready.pop() {
            n
        } else {
            let cycle = find_cycle(&indegree, provider_of, &by_name, &emitted).unwrap_or_default();
            let pick = cycle
                .iter()
                .min_by_key(|n| indegree.get(*n).copied().unwrap_or(0))
                .cloned();
            let Some(pick) = pick else { break };
            if !cycle.is_empty() {
                cycles.push(cycle);
            }
            pick
        };

        if !emitted.insert(n.clone()) {
            continue;
        }
        order.push(n.clone());

        if let Some(ds) = dependents.get(&n).cloned() {
            for dep in ds {
                if let Some(deg) = indegree.get_mut(&dep) {
                    *deg = deg.saturating_sub(1);
                    if *deg == 0 {
                        ready.push(Reverse(dep));
                    }
                }
            }
        }
    }

    (order, cycles)
}
