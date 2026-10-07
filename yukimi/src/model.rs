// SPDX-License-Identifier: MIT OR Apache-2.0
//! Everything Yukimi shows, read in one go away from the interface thread.
use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::path::Path;

use yukimi_config::{edit, lock::FlakeLock, lock::Input, packages};
use yukimi_store::{Graph, PathId, Root, RootKind, StoreDb, StorePath, roots};
use yukimi_system::catalog::{self, Application};
use yukimi_system::generations::{self, Generation};
use yukimi_system::info::SystemInfo;
use yukimi_system::profile::{self, Element};
use yukimi_system::{CONFIG_DIR, nix};

/// A package in the running system, as its store paths name it.
#[derive(Clone, Debug)]
pub struct SystemPackage {
    pub name: String,
    pub version: String,
    pub size: u64,
}

/// The store, divided by what keeps each part of it.
#[derive(Clone, Debug, Default)]
pub struct Composition {
    /// The running (and started) system.
    pub system: u64,
    /// Kept only by older system generations.
    pub old_generations: u64,
    /// Kept by users' own packages.
    pub profiles: u64,
    /// Kept by build results and development shells.
    pub projects: u64,
    /// Kept by nothing: garbage collection would free it.
    pub garbage: u64,
    pub total: u64,
    pub paths: usize,
    pub garbage_paths: usize,
}

/// A package in a store, by name, with the size of all its paths.
#[derive(Clone, Debug)]
pub struct Heavy {
    pub name: String,
    pub version: String,
    pub size: u64,
}

#[derive(Default)]
pub struct Model {
    pub info: SystemInfo,
    pub generations: Vec<Generation>,
    pub roots: Vec<Root>,
    pub graph: Graph,
    pub composition: Composition,
    /// Packages of the running system's `environment.systemPackages`.
    pub system_packages: Vec<SystemPackage>,
    /// The heaviest packages in the running system.
    pub heaviest: Vec<Heavy>,
    /// Packages the current user installed for themselves.
    pub user_packages: Vec<Element>,
    /// Applications chosen from the installer's catalog.
    pub applications: Vec<String>,
    /// Packages installed for everyone with Yukimi.
    pub yukimi_packages: Vec<String>,
    /// The system accepts unfree packages.
    pub allow_unfree: bool,
    pub catalog: Vec<Application>,
    pub inputs: Vec<Input>,
    /// Store path of the system's Nixpkgs, for searching it.
    pub nixpkgs: Option<String>,
    /// What could not be read, to say so instead of failing.
    pub problems: Vec<String>,
}

impl Model {
    pub fn load() -> Model {
        let mut model = Model {
            info: SystemInfo::read(),
            generations: generations::system(),
            roots: roots::scan(),
            user_packages: profile::user_elements(),
            ..Default::default()
        };
        match StoreDb::open_default().and_then(|db| db.graph()) {
            Ok(graph) => model.graph = graph,
            Err(e) => model.problems.push(format!("The store database could not be read: {e}")),
        }
        // The garbage collector keeps derivations of live paths unless told
        // not to; Nix's default is to keep them.
        let keep_derivations = nix::setting("keep-derivations").is_none_or(|value| value != "false");
        model.composition = composition(&model.graph, &model.roots, keep_derivations);
        model.system_packages = system_packages(&model.graph);
        model.heaviest = heaviest(&model.graph, 24);
        model.read_configuration();
        model.read_inputs();
        model
    }

    fn read_configuration(&mut self) {
        let dir = Path::new(CONFIG_DIR);
        match std::fs::read_to_string(dir.join("configuration.nix")) {
            Ok(text) => {
                match edit::string_list(&text, &["calamares", "applications"]) {
                    Ok(list) => self.applications = list.unwrap_or_default(),
                    Err(e) => self.problems.push(format!("calamares.applications: {e}")),
                }
                self.allow_unfree =
                    edit::bool_value(&text, &["nixpkgs", "config", "allowUnfree"]).ok().flatten().unwrap_or(false);
            }
            Err(e) => self.problems.push(format!("{CONFIG_DIR}/configuration.nix could not be read: {e}")),
        }
        if let Ok(text) = std::fs::read_to_string(dir.join(packages::FILE)) {
            match packages::read(&text) {
                Ok(list) => self.yukimi_packages = list,
                Err(e) => self.problems.push(format!("{}: {e}", packages::FILE)),
            }
        }
        match std::fs::read_to_string(dir.join("flake.lock")).map(|t| FlakeLock::parse(&t)) {
            Ok(Ok(lock)) => self.inputs = lock.inputs(),
            Ok(Err(e)) => self.problems.push(e.to_string()),
            Err(_) => {}
        }
    }

    /// The system flake's Nixpkgs and the installer's catalog. Asks Nix,
    /// which copies the configuration into the store the first time.
    fn read_inputs(&mut self) {
        match nix::system_inputs(CONFIG_DIR) {
            Ok(inputs) => {
                self.nixpkgs = inputs.nixpkgs;
                if let Some(source) = inputs.calamares {
                    let path = Path::new(&source).join(catalog::CATALOG_PATH);
                    if let Some(apps) = std::fs::read_to_string(path).ok().and_then(|t| catalog::parse(&t)) {
                        self.catalog = apps;
                    }
                }
            }
            Err(e) => self.problems.push(format!("The system's flake inputs could not be read: {e}")),
        }
    }

    pub fn current_generation(&self) -> Option<&Generation> {
        self.generations.iter().find(|g| g.current)
    }

    pub fn application(&self, id: &str) -> Option<&Application> {
        self.catalog.iter().find(|a| a.id == id)
    }

    /// Names of packages the running system has, for marking search results.
    /// Attributes the current user installed.
    pub fn user_attrs(&self) -> BTreeSet<&str> {
        self.user_packages.iter().filter_map(|e| e.package_attr()).collect()
    }

    /// The closure of a generation's build.
    pub fn closure_of(&self, target: &StorePath) -> Vec<PathId> {
        self.graph.lookup(target.as_str()).map(|id| self.graph.closure([id])).unwrap_or_default()
    }
}

fn ids(graph: &Graph, roots: &[&Root]) -> Vec<PathId> {
    roots.iter().filter_map(|r| graph.lookup(r.target.as_str())).collect()
}

fn composition(graph: &Graph, roots: &[Root], keep_derivations: bool) -> Composition {
    let mut owner: Vec<u8> = vec![0; graph.len()];
    let groups: [(u8, Vec<&Root>); 4] = [
        (1, roots.iter().filter(|r| matches!(r.kind, RootKind::CurrentSystem | RootKind::BootedSystem)).collect()),
        (2, roots.iter().filter(|r| matches!(r.kind, RootKind::SystemGeneration(_))).collect()),
        (3, roots.iter().filter(|r| matches!(r.kind, RootKind::Profile { .. })).collect()),
        (
            4,
            roots
                .iter()
                .filter(|r| matches!(r.kind, RootKind::BuildResult | RootKind::DevShell | RootKind::Other))
                .collect(),
        ),
    ];
    // If /run/current-system is not among the roots seen, the current system
    // generation stands in for it.
    let current = std::fs::read_link("/run/current-system").ok().and_then(|p| graph.lookup(p.to_str()?));
    for (tag, group) in &groups {
        let mut start = ids(graph, group);
        if *tag == 1 {
            start.extend(current);
        }
        for id in graph.kept(start, keep_derivations) {
            if owner[id.index()] == 0 {
                owner[id.index()] = *tag;
            }
        }
    }
    let mut c = Composition { paths: graph.len(), ..Default::default() };
    for id in graph.ids() {
        let size = graph.info(id).nar_size;
        c.total += size;
        match owner[id.index()] {
            1 => c.system += size,
            2 => c.old_generations += size,
            3 => c.profiles += size,
            4 => c.projects += size,
            _ => {
                c.garbage += size;
                c.garbage_paths += 1;
            }
        }
    }
    c
}

/// The packages in the running system's `environment.systemPackages`: what
/// its `sw` profile refers to.
fn system_packages(graph: &Graph) -> Vec<SystemPackage> {
    let Some(sw) = std::fs::read_link("/run/current-system/sw").ok().and_then(|p| graph.lookup(p.to_str()?)) else {
        return Vec::new();
    };
    let mut by_name: BTreeMap<String, SystemPackage> = BTreeMap::new();
    for &id in graph.references(sw) {
        let Some(path) = graph.store_path(id) else { continue };
        let entry = by_name.entry(path.name().to_owned()).or_insert_with(|| SystemPackage {
            name: path.name().to_owned(),
            version: path.version().to_owned(),
            size: 0,
        });
        entry.size += graph.info(id).nar_size;
    }
    by_name.into_values().collect()
}

/// The heaviest packages in the running system's closure, by name.
fn heaviest(graph: &Graph, count: usize) -> Vec<Heavy> {
    let Some(current) = std::fs::read_link("/run/current-system").ok().and_then(|p| graph.lookup(p.to_str()?)) else {
        return Vec::new();
    };
    let mut by_name: HashMap<String, Heavy> = HashMap::new();
    for id in graph.closure([current]) {
        let Some(path) = graph.store_path(id) else { continue };
        if path.is_derivation() {
            continue;
        }
        let entry = by_name.entry(path.name().to_owned()).or_insert_with(|| Heavy {
            name: path.name().to_owned(),
            version: path.version().to_owned(),
            size: 0,
        });
        entry.size += graph.info(id).nar_size;
    }
    let mut list: Vec<Heavy> = by_name.into_values().collect();
    list.sort_by_key(|h| std::cmp::Reverse(h.size));
    list.truncate(count);
    list
}
