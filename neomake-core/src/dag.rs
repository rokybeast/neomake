//! Dependency-graph construction and traversal.
//!
//! # Algorithm
//!
//! We use Kahn's algorithm for topological sorting:
//!
//! 1. Compute the in-degree of every node (task).
//! 2. Seed a queue with all nodes of in-degree zero.
//! 3. Repeatedly pop a node, append it to the topo order, and decrement
//!    the in-degree of each of its dependents, pushing any that reach
//!    zero back onto the queue.
//! 4. If the resulting order does not cover every node, at least one
//!    cycle exists. We locate a concrete cycle by running an iterative
//!    DFS over the unprocessed subgraph; the first back-edge encountered
//!    gives us the cycle to report to the user.
//!
//! Producing a concrete cycle path (rather than just a boolean) is what
//! lets the CLI print something like `a -> b -> c -> a`, fulfilling the
//! "clear error message identifying the cycle" requirement.

use std::collections::{BTreeMap, BTreeSet, VecDeque};

use crate::error::DagError;
use crate::task::Task;

/// A validated, acyclic task dependency graph.
#[derive(Debug, Clone)]
pub struct Dag {
    tasks: Vec<Task>,
    topo: Vec<String>,
    dependents: BTreeMap<String, Vec<String>>,
    deps: BTreeMap<String, Vec<String>>,
}

impl Dag {
    /// Build a [`Dag`] from a slice of tasks.
    ///
    /// Returns [`DagError::UnknownDependency`] if any task references a
    /// missing dep, or [`DagError::Cycle`] if the graph is cyclic.
    pub fn build(tasks: &[Task]) -> Result<Self, DagError> {
        let names: BTreeSet<&str> = tasks.iter().map(|t| t.name.as_str()).collect();

        // Validate that every referenced dep exists.
        for t in tasks {
            for d in &t.deps {
                if !names.contains(d.as_str()) {
                    return Err(DagError::UnknownDependency {
                        task: t.name.clone(),
                        missing: d.clone(),
                    });
                }
            }
        }

        // Build forward-edge (dep -> dependent) and reverse (task -> deps) maps.
        let mut dependents: BTreeMap<String, Vec<String>> = BTreeMap::new();
        let mut deps: BTreeMap<String, Vec<String>> = BTreeMap::new();
        let mut in_degree: BTreeMap<String, usize> = BTreeMap::new();
        for t in tasks {
            in_degree.entry(t.name.clone()).or_insert(0);
            deps.entry(t.name.clone()).or_default().clone_from(&t.deps);
            for d in &t.deps {
                dependents
                    .entry(d.clone())
                    .or_default()
                    .push(t.name.clone());
                *in_degree.entry(t.name.clone()).or_insert(0) += 1;
            }
            dependents.entry(t.name.clone()).or_default();
        }

        // Kahn's algorithm.
        let mut queue: VecDeque<String> = in_degree
            .iter()
            .filter_map(|(n, &d)| if d == 0 { Some(n.clone()) } else { None })
            .collect();
        let mut topo = Vec::with_capacity(tasks.len());
        let mut remaining = in_degree.clone();

        while let Some(n) = queue.pop_front() {
            topo.push(n.clone());
            if let Some(children) = dependents.get(&n) {
                for c in children {
                    if let Some(d) = remaining.get_mut(c) {
                        *d -= 1;
                        if *d == 0 {
                            queue.push_back(c.clone());
                        }
                    }
                }
            }
        }

        if topo.len() != tasks.len() {
            let still_live: BTreeSet<String> = remaining
                .into_iter()
                .filter(|(_, d)| *d > 0)
                .map(|(n, _)| n)
                .collect();
            let cycle = find_cycle(&still_live, &deps).unwrap_or_else(|| {
                // Fallback: should not happen, but we never panic in user paths.
                still_live.iter().cloned().collect::<Vec<_>>()
            });
            return Err(DagError::Cycle { path: cycle });
        }

        Ok(Self {
            tasks: tasks.to_vec(),
            topo,
            dependents,
            deps,
        })
    }

    /// Task names in topological (ready-first) order.
    pub fn topo_order(&self) -> &[String] {
        &self.topo
    }

    /// Tasks that depend on `name`.
    pub fn dependents(&self, name: &str) -> &[String] {
        self.dependents.get(name).map(Vec::as_slice).unwrap_or(&[])
    }

    /// Dependencies declared by `name`.
    pub fn deps_of(&self, name: &str) -> &[String] {
        self.deps.get(name).map(Vec::as_slice).unwrap_or(&[])
    }

    /// All tasks (in original declaration order).
    pub fn tasks(&self) -> &[Task] {
        &self.tasks
    }
}

/// Find a concrete cycle using iterative DFS (tri-coloring).
fn find_cycle(
    live: &BTreeSet<String>,
    deps: &BTreeMap<String, Vec<String>>,
) -> Option<Vec<String>> {
    // 0 = unvisited, 1 = on stack, 2 = done
    let mut color: BTreeMap<String, u8> = live.iter().map(|n| (n.clone(), 0)).collect();

    for start in live {
        if color[start] != 0 {
            continue;
        }
        let mut stack: Vec<(String, usize)> = Vec::new();
        let mut path: Vec<String> = Vec::new();
        stack.push((start.clone(), 0));
        color.insert(start.clone(), 1);
        path.push(start.clone());

        while let Some((node, idx)) = stack.last().cloned() {
            let empty = Vec::new();
            let node_deps = deps.get(&node).unwrap_or(&empty);
            if idx >= node_deps.len() {
                color.insert(node.clone(), 2);
                stack.pop();
                path.pop();
                continue;
            }
            // Advance the iterator for the current frame.
            stack.last_mut().unwrap().1 = idx + 1;

            let child = &node_deps[idx];
            if !live.contains(child) {
                continue;
            }
            match color.get(child).copied().unwrap_or(0) {
                0 => {
                    color.insert(child.clone(), 1);
                    path.push(child.clone());
                    stack.push((child.clone(), 0));
                }
                1 => {
                    // Back-edge: slice `path` from `child` to form the cycle.
                    if let Some(pos) = path.iter().position(|n| n == child) {
                        let mut cyc: Vec<String> = path[pos..].to_vec();
                        cyc.push(child.clone());
                        return Some(cyc);
                    }
                }
                _ => {}
            }
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeMap;

    fn task(name: &str, deps: &[&str]) -> Task {
        Task {
            name: name.into(),
            command: "true".into(),
            deps: deps.iter().map(|s| s.to_string()).collect(),
            inputs: vec![],
            outputs: vec![],
            env: BTreeMap::new(),
        }
    }

    #[test]
    fn topo_orders_diamond() {
        //   a
        //  / \
        // b   c
        //  \ /
        //   d
        let tasks = vec![
            task("a", &[]),
            task("b", &["a"]),
            task("c", &["a"]),
            task("d", &["b", "c"]),
        ];
        let dag = Dag::build(&tasks).unwrap();
        let order = dag.topo_order();
        let pos = |n: &str| order.iter().position(|x| x == n).unwrap();
        assert!(pos("a") < pos("b"));
        assert!(pos("a") < pos("c"));
        assert!(pos("b") < pos("d"));
        assert!(pos("c") < pos("d"));
    }

    #[test]
    fn detects_unknown_dep() {
        let tasks = vec![task("a", &["ghost"])];
        let err = Dag::build(&tasks).unwrap_err();
        match err {
            DagError::UnknownDependency { task, missing } => {
                assert_eq!(task, "a");
                assert_eq!(missing, "ghost");
            }
            _ => panic!("expected UnknownDependency"),
        }
    }

    #[test]
    fn detects_cycle_path() {
        // a -> b -> c -> a
        let tasks = vec![task("a", &["c"]), task("b", &["a"]), task("c", &["b"])];
        let err = Dag::build(&tasks).unwrap_err();
        match err {
            DagError::Cycle { path } => {
                assert!(path.len() >= 2);
                assert_eq!(path.first(), path.last());
                // The cycle members must all be in {a,b,c}.
                for n in &path {
                    assert!(["a", "b", "c"].contains(&n.as_str()), "node {n}");
                }
            }
            _ => panic!("expected Cycle"),
        }
    }

    #[test]
    fn detects_self_loop() {
        let tasks = vec![task("a", &["a"])];
        let err = Dag::build(&tasks).unwrap_err();
        assert!(matches!(err, DagError::Cycle { .. }));
    }
}
