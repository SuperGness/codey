use std::collections::{BTreeMap, BTreeSet, HashMap, VecDeque};

use serde::{Deserialize, Serialize};

use super::domain::{
    NodeId, NodeRecord, NodeRole, NodeStatus, PermissionSet, PolicySnapshot, RouteMode,
    WorkflowError, WorkflowResult,
};

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct NodeSpec {
    pub id: NodeId,
    pub role: NodeRole,
    pub label: String,
    pub dependencies: Vec<NodeId>,
    pub permissions: PermissionSet,
    pub repo_writer: bool,
    pub provider: String,
    pub depth: u8,
    pub max_attempts: u32,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WorkflowPlan {
    pub route: RouteMode,
    pub nodes: Vec<NodeSpec>,
}

impl WorkflowPlan {
    pub fn validate(&self, max_depth: u8) -> WorkflowResult<()> {
        let ids: BTreeSet<_> = self.nodes.iter().map(|node| node.id.as_str()).collect();
        if ids.len() != self.nodes.len() {
            return Err(WorkflowError::InvalidRequest(
                "workflow plan contains duplicate node ids".to_string(),
            ));
        }
        for node in &self.nodes {
            if node.depth > max_depth {
                return Err(WorkflowError::InvalidRequest(format!(
                    "node {} exceeds delegation depth {max_depth}",
                    node.id
                )));
            }
            if node
                .dependencies
                .iter()
                .any(|dependency| !ids.contains(dependency.as_str()))
            {
                return Err(WorkflowError::InvalidRequest(format!(
                    "node {} references an unknown dependency",
                    node.id
                )));
            }
            if node
                .dependencies
                .iter()
                .any(|dependency| dependency == &node.id)
            {
                return Err(WorkflowError::InvalidRequest(format!(
                    "node {} depends on itself",
                    node.id
                )));
            }
        }

        let mut indegree: HashMap<&str, usize> = self
            .nodes
            .iter()
            .map(|node| (node.id.as_str(), node.dependencies.len()))
            .collect();
        let mut dependents: HashMap<&str, Vec<&str>> = HashMap::new();
        for node in &self.nodes {
            for dependency in &node.dependencies {
                dependents.entry(dependency).or_default().push(&node.id);
            }
        }
        let mut queue: VecDeque<&str> = indegree
            .iter()
            .filter_map(|(id, degree)| (*degree == 0).then_some(*id))
            .collect();
        let mut visited = 0;
        while let Some(id) = queue.pop_front() {
            visited += 1;
            for dependent in dependents.get(id).into_iter().flatten() {
                let degree = indegree.get_mut(dependent).expect("validated node id");
                *degree -= 1;
                if *degree == 0 {
                    queue.push_back(dependent);
                }
            }
        }
        if visited != self.nodes.len() {
            return Err(WorkflowError::InvalidRequest(
                "workflow plan must be acyclic".to_string(),
            ));
        }
        Ok(())
    }
}

#[derive(Debug, Clone)]
pub struct PlanCompiler {
    policy: PolicySnapshot,
}

impl PlanCompiler {
    pub fn new(policy: PolicySnapshot) -> Self {
        Self { policy }
    }

    pub fn compile(&self, route: RouteMode) -> WorkflowResult<WorkflowPlan> {
        let permissions = &self.policy.permissions;
        let read_attempts = 1 + u32::from(self.policy.infrastructure_retry_limit);
        let builder_attempts = read_attempts + u32::from(self.policy.builder_repair_limit);
        let read_only = PermissionSet {
            write_paths: BTreeSet::new(),
            ..permissions.clone()
        };
        let mut nodes = match route {
            RouteMode::Direct => vec![
                spec(
                    "builder",
                    NodeRole::Builder,
                    &[],
                    permissions.clone(),
                    !permissions.write_paths.is_empty(),
                    0,
                    builder_attempts,
                ),
                spec(
                    "final_delivery",
                    NodeRole::FinalDelivery,
                    &["builder"],
                    read_only,
                    false,
                    0,
                    1,
                ),
            ],
            RouteMode::Guarded => vec![
                spec(
                    "preflight",
                    NodeRole::Preflight,
                    &[],
                    read_only.clone(),
                    false,
                    0,
                    read_attempts,
                ),
                spec(
                    "builder",
                    NodeRole::Builder,
                    &["preflight"],
                    permissions.clone(),
                    !permissions.write_paths.is_empty(),
                    0,
                    builder_attempts,
                ),
                spec(
                    "validator",
                    NodeRole::Validator,
                    &["builder"],
                    read_only.clone(),
                    false,
                    0,
                    read_attempts,
                ),
                spec(
                    "reviewer",
                    NodeRole::Reviewer,
                    &["preflight", "builder", "validator"],
                    read_only.clone(),
                    false,
                    0,
                    read_attempts,
                ),
                spec(
                    "final_delivery",
                    NodeRole::FinalDelivery,
                    &["reviewer"],
                    read_only,
                    false,
                    0,
                    1,
                ),
            ],
            RouteMode::Parallel => {
                let width = usize::from(self.policy.max_read_only_concurrency.min(4));
                let mut nodes = vec![spec(
                    "preflight",
                    NodeRole::Preflight,
                    &[],
                    read_only.clone(),
                    false,
                    0,
                    read_attempts,
                )];
                for index in 0..width {
                    nodes.push(spec(
                        &format!("research_{}", index + 1),
                        NodeRole::Scout,
                        &["preflight"],
                        read_only.clone(),
                        false,
                        1,
                        read_attempts,
                    ));
                }
                let research: Vec<String> = (0..width)
                    .map(|index| format!("research_{}", index + 1))
                    .collect();
                nodes.push(spec_owned(
                    "builder",
                    NodeRole::Builder,
                    research,
                    permissions.clone(),
                    !permissions.write_paths.is_empty(),
                    0,
                    builder_attempts,
                ));
                nodes.push(spec(
                    "validator",
                    NodeRole::Validator,
                    &["builder"],
                    read_only.clone(),
                    false,
                    0,
                    read_attempts,
                ));
                nodes.push(spec(
                    "reviewer",
                    NodeRole::Reviewer,
                    &["preflight", "builder", "validator"],
                    read_only.clone(),
                    false,
                    0,
                    read_attempts,
                ));
                nodes.push(spec(
                    "final_delivery",
                    NodeRole::FinalDelivery,
                    &["reviewer"],
                    read_only,
                    false,
                    0,
                    1,
                ));
                nodes
            }
            RouteMode::Expert => vec![
                spec(
                    "preflight",
                    NodeRole::Preflight,
                    &[],
                    read_only.clone(),
                    false,
                    0,
                    read_attempts,
                ),
                spec(
                    "expert_analysis",
                    NodeRole::Expert,
                    &["preflight"],
                    read_only.clone(),
                    false,
                    1,
                    read_attempts,
                ),
                spec(
                    "expert_risk",
                    NodeRole::Expert,
                    &["preflight"],
                    read_only.clone(),
                    false,
                    1,
                    read_attempts,
                ),
                spec(
                    "integrator",
                    NodeRole::Integrator,
                    &["expert_analysis", "expert_risk"],
                    read_only.clone(),
                    false,
                    0,
                    read_attempts,
                ),
                spec(
                    "builder",
                    NodeRole::Builder,
                    &["integrator"],
                    permissions.clone(),
                    !permissions.write_paths.is_empty(),
                    0,
                    builder_attempts,
                ),
                spec(
                    "validator",
                    NodeRole::Validator,
                    &["builder"],
                    read_only.clone(),
                    false,
                    0,
                    read_attempts,
                ),
                spec(
                    "reviewer_correctness",
                    NodeRole::Reviewer,
                    &["preflight", "integrator", "builder", "validator"],
                    read_only.clone(),
                    false,
                    0,
                    read_attempts,
                ),
                spec(
                    "reviewer_risk",
                    NodeRole::Reviewer,
                    &["preflight", "integrator", "builder", "validator"],
                    read_only.clone(),
                    false,
                    0,
                    read_attempts,
                ),
                spec(
                    "final_delivery",
                    NodeRole::FinalDelivery,
                    &["reviewer_correctness", "reviewer_risk"],
                    read_only,
                    false,
                    0,
                    1,
                ),
            ],
        };
        // Permission snapshots on nodes are always narrowed against the run
        // snapshot, including plans supplied by a future compiler version.
        for node in &mut nodes {
            node.permissions = self.policy.permissions.intersect(&node.permissions);
        }
        let plan = WorkflowPlan { route, nodes };
        plan.validate(self.policy.max_delegation_depth)?;
        Ok(plan)
    }
}

fn spec(
    id: &str,
    role: NodeRole,
    dependencies: &[&str],
    permissions: PermissionSet,
    repo_writer: bool,
    depth: u8,
    max_attempts: u32,
) -> NodeSpec {
    spec_owned(
        id,
        role,
        dependencies
            .iter()
            .map(|value| (*value).to_string())
            .collect(),
        permissions,
        repo_writer,
        depth,
        max_attempts,
    )
}

fn spec_owned(
    id: &str,
    role: NodeRole,
    dependencies: Vec<String>,
    permissions: PermissionSet,
    repo_writer: bool,
    depth: u8,
    max_attempts: u32,
) -> NodeSpec {
    NodeSpec {
        id: id.to_string(),
        role,
        label: id.replace('_', " "),
        dependencies,
        permissions,
        repo_writer,
        provider: "default".to_string(),
        depth,
        max_attempts,
    }
}

#[derive(Debug, Default)]
pub struct Scheduler {
    read_only_in_use: u16,
    provider_in_use: BTreeMap<String, u16>,
    repo_writers_in_use: u16,
}

impl Scheduler {
    pub fn ready_nodes<'a>(
        &self,
        nodes: &'a [NodeRecord],
        policy: &PolicySnapshot,
    ) -> Vec<&'a NodeRecord> {
        let statuses: HashMap<&str, NodeStatus> = nodes
            .iter()
            .map(|node| (node.id.as_str(), node.status))
            .collect();
        nodes
            .iter()
            .filter(|node| node.status == NodeStatus::Ready)
            .filter(|node| {
                node.dependencies.iter().all(|dependency| {
                    matches!(
                        statuses.get(dependency.as_str()),
                        Some(NodeStatus::Succeeded | NodeStatus::Skipped | NodeStatus::Compensated)
                    )
                })
            })
            .filter(|node| self.has_capacity(node, policy))
            .collect()
    }

    pub fn reserve(&mut self, node: &NodeRecord, policy: &PolicySnapshot) -> WorkflowResult<()> {
        if !self.has_capacity(node, policy) {
            return Err(WorkflowError::Conflict(format!(
                "no scheduler capacity for node {}",
                node.id
            )));
        }
        if node.repo_writer {
            self.repo_writers_in_use += 1;
        } else {
            self.read_only_in_use += 1;
        }
        *self
            .provider_in_use
            .entry(node.provider.clone())
            .or_default() += 1;
        Ok(())
    }

    pub fn release(&mut self, node: &NodeRecord) {
        if node.repo_writer {
            self.repo_writers_in_use = self.repo_writers_in_use.saturating_sub(1);
        } else {
            self.read_only_in_use = self.read_only_in_use.saturating_sub(1);
        }
        if let Some(in_use) = self.provider_in_use.get_mut(&node.provider) {
            *in_use = in_use.saturating_sub(1);
            if *in_use == 0 {
                self.provider_in_use.remove(&node.provider);
            }
        }
    }

    fn has_capacity(&self, node: &NodeRecord, policy: &PolicySnapshot) -> bool {
        let class_available = if node.repo_writer {
            self.repo_writers_in_use < policy.max_repo_writers
        } else {
            self.read_only_in_use < policy.max_read_only_concurrency
        };
        let provider_available = self
            .provider_in_use
            .get(&node.provider)
            .copied()
            .unwrap_or(0)
            < policy.max_provider_concurrency;
        class_available && provider_available && node.depth <= policy.max_delegation_depth
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::workflow::domain::{PolicySnapshot, WorkspaceRisk};

    fn snapshot() -> PolicySnapshot {
        PolicySnapshot {
            version: 1,
            max_read_only_concurrency: 4,
            max_provider_concurrency: 2,
            max_repo_writers: 1,
            max_delegation_depth: 1,
            infrastructure_retry_limit: 3,
            builder_repair_limit: 2,
            permissions: PermissionSet::default(),
            workspace_risk: WorkspaceRisk::default(),
        }
    }

    #[test]
    fn all_modes_compile_to_valid_dags() {
        let compiler = PlanCompiler::new(snapshot());
        for route in [
            RouteMode::Direct,
            RouteMode::Guarded,
            RouteMode::Parallel,
            RouteMode::Expert,
        ] {
            let plan = compiler.compile(route).unwrap();
            plan.validate(1).unwrap();
            assert_eq!(plan.nodes.last().unwrap().role, NodeRole::FinalDelivery);
        }
    }

    #[test]
    fn parallel_mode_has_parallel_research_branches() {
        let plan = PlanCompiler::new(snapshot())
            .compile(RouteMode::Parallel)
            .unwrap();
        assert_eq!(
            plan.nodes
                .iter()
                .filter(|node| node.role == NodeRole::Scout)
                .count(),
            4
        );
        let builder = plan
            .nodes
            .iter()
            .find(|node| node.role == NodeRole::Builder)
            .unwrap();
        assert_eq!(builder.dependencies.len(), 4);
    }

    #[test]
    fn review_context_and_high_risk_double_review_are_explicit() {
        let compiler = PlanCompiler::new(snapshot());
        let guarded = compiler.compile(RouteMode::Guarded).unwrap();
        let reviewer = guarded
            .nodes
            .iter()
            .find(|node| node.role == NodeRole::Reviewer)
            .unwrap();
        assert_eq!(
            reviewer.dependencies,
            vec!["preflight", "builder", "validator"]
        );

        let expert = compiler.compile(RouteMode::Expert).unwrap();
        assert_eq!(
            expert
                .nodes
                .iter()
                .filter(|node| node.role == NodeRole::Reviewer)
                .count(),
            2
        );
        let final_node = expert.nodes.last().unwrap();
        assert_eq!(final_node.dependencies.len(), 2);
    }

    #[test]
    fn scheduler_capacity_is_shared_by_reservations() {
        let policy = snapshot();
        let plan = PlanCompiler::new(policy.clone())
            .compile(RouteMode::Parallel)
            .unwrap();
        let mut nodes = plan
            .nodes
            .into_iter()
            .filter(|node| node.role == NodeRole::Scout)
            .map(|spec| NodeRecord {
                run_id: "run".to_string(),
                id: spec.id,
                role: spec.role,
                label: spec.label,
                status: NodeStatus::Ready,
                revision: 0,
                generation: 0,
                lease_epoch: 0,
                lease_owner: None,
                lease_expires_at_ms: None,
                payload_hash: "hash".to_string(),
                dependencies: Vec::new(),
                permissions: spec.permissions,
                repo_writer: spec.repo_writer,
                provider: spec.provider,
                depth: spec.depth,
                attempt_count: 0,
                max_attempts: spec.max_attempts,
                result: None,
                thread_id: None,
                updated_at_ms: 0,
            })
            .collect::<Vec<_>>();
        nodes.truncate(3);
        let mut scheduler = Scheduler::default();
        scheduler.reserve(&nodes[0], &policy).unwrap();
        scheduler.reserve(&nodes[1], &policy).unwrap();
        assert!(scheduler.reserve(&nodes[2], &policy).is_err());
        scheduler.release(&nodes[0]);
        assert!(scheduler.reserve(&nodes[2], &policy).is_ok());
    }
}
