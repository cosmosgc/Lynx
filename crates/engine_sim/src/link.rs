//! Typed links between stable entity IDs.
//!
//! Links are authored, validated references. Missing endpoints produce
//! diagnostics and never execute silently. See the project guide's linked
//! entity rules and `docs/adr/0010` for the activation budget.

use std::collections::{BTreeMap, BTreeSet};

use engine_core::error::{EngineError, ErrorCode};
use engine_core::id::{DefinitionId, StableId};
use engine_core::schema::PersistencePolicy;
use serde::{Deserialize, Serialize};

/// Events a link can carry.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LinkEvent {
    /// A general activation, such as a lever pull.
    Activated,
    /// A door or container was opened.
    Opened,
    /// A door or container was closed.
    Closed,
    /// A pressure plate or sensor fired.
    Triggered,
    /// An AI requested nearby assistance.
    HelpRequested,
}

impl LinkEvent {
    /// Wire string.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Activated => "activated",
            Self::Opened => "opened",
            Self::Closed => "closed",
            Self::Triggered => "triggered",
            Self::HelpRequested => "help_requested",
        }
    }
}

/// A typed, directional connection between two entities.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct EntityLink {
    /// Link type, for example `link.lever_to_door`.
    pub link_type: DefinitionId,
    /// Entity that emits the event.
    pub source: StableId,
    /// Entity that receives the event.
    pub target: StableId,
    /// Events carried by this link.
    pub events: BTreeSet<LinkEvent>,
    /// Whether the link is active.
    pub enabled: bool,
    /// Whether the target is in another zone and requires an explicit route.
    pub cross_zone: bool,
    /// Persistence policy for the link record.
    pub persistence: PersistencePolicy,
    /// Server ticks before the same link may fire again.
    pub cooldown_ticks: u64,
}

impl EntityLink {
    /// Create an enabled, persistent in-zone link.
    #[must_use]
    pub fn new(
        link_type: DefinitionId,
        source: StableId,
        target: StableId,
        events: impl IntoIterator<Item = LinkEvent>,
    ) -> Self {
        Self {
            link_type,
            source,
            target,
            events: events.into_iter().collect(),
            enabled: true,
            cross_zone: false,
            persistence: PersistencePolicy::PersistentEntity,
            cooldown_ticks: 0,
        }
    }

    /// Whether the link carries `event`.
    #[must_use]
    pub fn carries(&self, event: LinkEvent) -> bool {
        self.enabled && self.events.contains(&event)
    }
}

/// A structural problem with the link graph.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LinkDiagnostic {
    /// Stable diagnostic code.
    pub code: String,
    /// Source entity, when known.
    pub source: Option<StableId>,
    /// Target entity, when known.
    pub target: Option<StableId>,
    /// Human-readable explanation.
    pub message: String,
}

/// The set of authored links in a zone.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct LinkGraph {
    links: Vec<EntityLink>,
}

impl LinkGraph {
    /// An empty graph.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Number of links.
    #[must_use]
    pub fn len(&self) -> usize {
        self.links.len()
    }

    /// Whether the graph is empty.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.links.is_empty()
    }

    /// All links.
    #[must_use]
    pub fn links(&self) -> &[EntityLink] {
        &self.links
    }

    /// Add a link.
    ///
    /// # Errors
    ///
    /// Returns [`ErrorCode::InvalidRequest`] for an empty event set, a
    /// self-link, or a cycle; [`ErrorCode::Conflict`] for an exact duplicate.
    pub fn add(&mut self, link: EntityLink) -> Result<(), EngineError> {
        if link.events.is_empty() {
            return Err(EngineError::new(
                ErrorCode::InvalidRequest,
                "a link must carry at least one event",
            )
            .with_detail("link_type", link.link_type.to_string()));
        }
        if link.source == link.target {
            return Err(EngineError::new(
                ErrorCode::InvalidRequest,
                "a link cannot target its own source",
            )
            .with_detail("entity", link.source.to_string()));
        }
        if self.links.iter().any(|existing| {
            existing.link_type == link.link_type
                && existing.source == link.source
                && existing.target == link.target
        }) {
            return Err(EngineError::new(ErrorCode::Conflict, "link already exists")
                .with_detail("source", link.source.to_string())
                .with_detail("target", link.target.to_string()));
        }

        self.links.push(link);
        if let Some(cycle) = self.find_cycle() {
            self.links.pop();
            return Err(EngineError::new(
                ErrorCode::InvalidRequest,
                "link would create an activation cycle",
            )
            .with_detail(
                "cycle",
                cycle
                    .iter()
                    .map(ToString::to_string)
                    .collect::<Vec<_>>()
                    .join(" -> "),
            ));
        }
        Ok(())
    }

    /// Remove every link touching `entity`, returning how many were removed.
    pub fn remove_for(&mut self, entity: StableId) -> usize {
        let before = self.links.len();
        self.links
            .retain(|link| link.source != entity && link.target != entity);
        before - self.links.len()
    }

    /// Targets of enabled in-zone links from `source` carrying `event`.
    #[must_use]
    pub fn targets(&self, source: StableId, event: LinkEvent) -> Vec<StableId> {
        self.links
            .iter()
            .filter(|link| link.source == source && link.carries(event) && !link.cross_zone)
            .map(|link| link.target)
            .collect()
    }

    /// The link type used by `source -> target` for `event`, if any.
    #[must_use]
    pub fn link_type_for(
        &self,
        source: StableId,
        target: StableId,
        event: LinkEvent,
    ) -> Option<&DefinitionId> {
        self.links
            .iter()
            .find(|link| link.source == source && link.target == target && link.carries(event))
            .map(|link| &link.link_type)
    }

    /// Validate endpoints and structure against the set of existing entities.
    #[must_use]
    pub fn diagnose(&self, exists: impl Fn(StableId) -> bool) -> Vec<LinkDiagnostic> {
        let mut diagnostics = Vec::new();
        for link in &self.links {
            if !exists(link.source) {
                diagnostics.push(LinkDiagnostic {
                    code: "link.missing_source".into(),
                    source: Some(link.source),
                    target: Some(link.target),
                    message: "link source does not exist in this zone".into(),
                });
            }
            if !exists(link.target) {
                diagnostics.push(LinkDiagnostic {
                    code: "link.missing_target".into(),
                    source: Some(link.source),
                    target: Some(link.target),
                    message: "link target does not exist in this zone".into(),
                });
            }
            if link.cross_zone {
                diagnostics.push(LinkDiagnostic {
                    code: "link.cross_zone_requires_route".into(),
                    source: Some(link.source),
                    target: Some(link.target),
                    message: "cross-zone link requires an explicit message route".into(),
                });
            }
        }
        if let Some(cycle) = self.find_cycle() {
            diagnostics.push(LinkDiagnostic {
                code: "link.cycle".into(),
                source: cycle.first().copied(),
                target: cycle.last().copied(),
                message: format!(
                    "activation cycle: {}",
                    cycle
                        .iter()
                        .map(ToString::to_string)
                        .collect::<Vec<_>>()
                        .join(" -> ")
                ),
            });
        }
        diagnostics
    }

    /// Find an activation cycle, if one exists.
    #[must_use]
    pub fn find_cycle(&self) -> Option<Vec<StableId>> {
        let mut adjacency: BTreeMap<StableId, Vec<StableId>> = BTreeMap::new();
        for link in &self.links {
            if link.enabled
                && !link.cross_zone
                && (link.events.contains(&LinkEvent::Activated)
                    || link.events.contains(&LinkEvent::Triggered))
            {
                adjacency.entry(link.source).or_default().push(link.target);
            }
        }

        let mut visited = BTreeSet::new();
        let mut stack = BTreeSet::new();
        let mut path = Vec::new();
        let starts: Vec<_> = adjacency.keys().copied().collect();
        for start in starts {
            if let Some(cycle) =
                find_cycle_from(start, &adjacency, &mut visited, &mut stack, &mut path, 0)
            {
                return Some(cycle);
            }
        }
        None
    }
}

fn find_cycle_from(
    node: StableId,
    adjacency: &BTreeMap<StableId, Vec<StableId>>,
    visited: &mut BTreeSet<StableId>,
    stack: &mut BTreeSet<StableId>,
    path: &mut Vec<StableId>,
    depth: usize,
) -> Option<Vec<StableId>> {
    // A generous cap keeps malformed graphs from exhausting the stack while
    // allowing sane authored chains.
    if depth > 128 {
        return None;
    }
    if !stack.insert(node) {
        let start = path.iter().position(|id| *id == node).unwrap_or(0);
        let mut cycle = path[start..].to_vec();
        cycle.push(node);
        return Some(cycle);
    }
    path.push(node);

    if let Some(neighbours) = adjacency.get(&node) {
        for &next in neighbours {
            if let Some(cycle) = find_cycle_from(next, adjacency, visited, stack, path, depth + 1) {
                return Some(cycle);
            }
        }
    }

    path.pop();
    stack.remove(&node);
    visited.insert(node);
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use engine_core::id::IdKind;

    fn id(seed: u128) -> StableId {
        StableId::from_parts(IdKind::Entity, 1, seed)
    }

    fn link_type() -> DefinitionId {
        DefinitionId::new("link.lever_to_door").unwrap()
    }

    #[test]
    fn links_carry_only_declared_events() {
        let link = EntityLink::new(link_type(), id(1), id(2), [LinkEvent::Activated]);
        assert!(link.carries(LinkEvent::Activated));
        assert!(!link.carries(LinkEvent::Opened));
    }

    #[test]
    fn duplicate_and_self_links_are_rejected() {
        let mut graph = LinkGraph::new();
        graph
            .add(EntityLink::new(
                link_type(),
                id(1),
                id(2),
                [LinkEvent::Activated],
            ))
            .unwrap();
        assert_eq!(
            graph
                .add(EntityLink::new(
                    link_type(),
                    id(1),
                    id(2),
                    [LinkEvent::Activated]
                ))
                .unwrap_err()
                .code(),
            ErrorCode::Conflict
        );
        assert_eq!(
            graph
                .add(EntityLink::new(
                    link_type(),
                    id(3),
                    id(3),
                    [LinkEvent::Activated]
                ))
                .unwrap_err()
                .code(),
            ErrorCode::InvalidRequest
        );
    }

    #[test]
    fn cycles_are_detected_and_refused() {
        let mut graph = LinkGraph::new();
        graph
            .add(EntityLink::new(
                link_type(),
                id(1),
                id(2),
                [LinkEvent::Activated],
            ))
            .unwrap();
        graph
            .add(EntityLink::new(
                link_type(),
                id(2),
                id(3),
                [LinkEvent::Triggered],
            ))
            .unwrap();
        let error = graph
            .add(EntityLink::new(
                link_type(),
                id(3),
                id(1),
                [LinkEvent::Activated],
            ))
            .unwrap_err();
        assert!(error.message().contains("cycle"));
        assert_eq!(graph.len(), 2);
    }

    #[test]
    fn links_are_removed_with_their_endpoints() {
        let mut graph = LinkGraph::new();
        graph
            .add(EntityLink::new(
                link_type(),
                id(1),
                id(2),
                [LinkEvent::Activated],
            ))
            .unwrap();
        graph
            .add(EntityLink::new(
                link_type(),
                id(2),
                id(3),
                [LinkEvent::Activated],
            ))
            .unwrap();
        assert_eq!(graph.remove_for(id(2)), 2);
        assert!(graph.is_empty());
    }

    #[test]
    fn missing_endpoints_are_diagnosed_not_executed() {
        let mut graph = LinkGraph::new();
        graph
            .add(EntityLink::new(
                link_type(),
                id(1),
                id(2),
                [LinkEvent::Activated],
            ))
            .unwrap();
        let diagnostics = graph.diagnose(|entity| entity == id(1));
        assert!(diagnostics.iter().any(|d| d.code == "link.missing_target"));
        assert_eq!(graph.targets(id(1), LinkEvent::Activated), vec![id(2)]);
    }

    #[test]
    fn disabled_links_do_not_fire_or_form_cycles() {
        let mut graph = LinkGraph::new();
        let mut forward = EntityLink::new(link_type(), id(1), id(2), [LinkEvent::Activated]);
        forward.enabled = false;
        graph.add(forward).unwrap();
        graph
            .add(EntityLink::new(
                link_type(),
                id(2),
                id(1),
                [LinkEvent::Activated],
            ))
            .unwrap();
        assert_eq!(
            graph.targets(id(1), LinkEvent::Activated),
            Vec::<StableId>::new()
        );
        assert!(graph.find_cycle().is_none());
    }
}
