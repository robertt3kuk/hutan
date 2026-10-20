//! The list model of the Agents page (chrome v3 § 2.4): which rows the attention-first list has
//! for a [`AgentPresentation`], under a scope, a search, an optional state filter, *Group by
//! workspace* and the folds the user opened. GPUI-free; the page only draws what this returns,
//! and keyboard navigation is a function of the rows, so it is the same at 3 and at 1000
//! sessions.

use std::collections::{BTreeMap, HashSet};

use hutan_core::{SessionId, WorkspaceId};

use super::presentation::{AgentPresentation, Counts, Group, Link, Scope};

/// What the list is filtered by.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Query {
    pub scope: Scope,
    /// Words that all have to match the title, harness, model, project, branch or activity.
    pub text: String,
    /// Only this group (the Today rail's *State unavailable* link).
    pub group: Option<Group>,
    /// Workspace headings as outer groups.
    pub by_workspace: bool,
    /// The *Finished* group is unfolded.
    pub finished_open: bool,
}

/// A row of the list.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Row {
    /// A group heading; `foldable` for *Finished*.
    Group {
        group: Group,
        count: usize,
        /// Needs-you only: how many requests wait in the group's sessions.
        requests: usize,
        foldable: bool,
        open: bool,
    },
    /// An outer heading of *Group by workspace*.
    Workspace {
        id: WorkspaceId,
        name: String,
        count: usize,
    },
    /// A session (its record is `AgentPresentation::get(session)`).
    Agent { session: SessionId },
    /// A subagent of the session above it, by its node id.
    Child { session: SessionId, node: String },
}

/// What a row is called across updates: a domain id, never a row number, so the selection and
/// the scroll anchor survive rows coming and going.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum Key {
    Group(Option<WorkspaceId>, Group),
    Agent(SessionId),
    Child(SessionId, String),
}

impl Row {
    /// Whether the keyboard can rest on the row.
    pub fn selectable(&self) -> bool {
        match self {
            Row::Group { foldable, .. } => *foldable,
            Row::Workspace { .. } => false,
            Row::Agent { .. } | Row::Child { .. } => true,
        }
    }
}

/// The rows of a query, with what the header says.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Listing {
    pub rows: Vec<Row>,
    /// Row keys, parallel to `rows` (`None` for a row without one).
    pub keys: Vec<Option<Key>>,
    /// The sessions of the scope, before the search and the state filter.
    pub in_scope: Counts,
    /// The sessions the rows are for.
    pub shown: usize,
}

impl Listing {
    pub fn position(&self, key: &Key) -> Option<usize> {
        self.keys.iter().position(|k| k.as_ref() == Some(key))
    }

    pub fn key(&self, index: usize) -> Option<&Key> {
        self.keys.get(index).and_then(Option::as_ref)
    }

    /// The selectable row after `from` (before it when `delta` is negative), `steps` of them;
    /// clamped at the ends.
    pub fn step(&self, from: Option<usize>, delta: isize, steps: usize) -> Option<usize> {
        let selectable: Vec<usize> = (0..self.rows.len())
            .filter(|i| self.rows[*i].selectable())
            .collect();
        if selectable.is_empty() {
            return None;
        }
        let at = from.and_then(|f| selectable.iter().position(|i| *i == f));
        let target = match (at, delta.signum()) {
            (None, d) if d >= 0 => 0,
            (None, _) => selectable.len() - 1,
            (Some(at), 1) => (at + steps).min(selectable.len() - 1),
            (Some(at), _) => at.saturating_sub(steps),
        };
        Some(selectable[target])
    }

    pub fn first(&self) -> Option<usize> {
        self.rows.iter().position(Row::selectable)
    }

    pub fn last(&self) -> Option<usize> {
        self.rows.iter().rposition(Row::selectable)
    }
}

/// Whether every word of `text` is in `haystack` (already lower case).
fn matches_text(haystack: &str, text: &str) -> bool {
    text.split_whitespace()
        .all(|word| haystack.contains(&word.to_lowercase()))
}

/// The rows for `query`; subagents of the sessions in `open` are listed under them.
pub fn list(presentation: &AgentPresentation, query: &Query, open: &HashSet<SessionId>) -> Listing {
    let in_scope = presentation.counts(query.scope);
    let mut included: Vec<usize> = Vec::new();
    for (index, record) in presentation.records.iter().enumerate() {
        if !query.scope.contains(record) {
            continue;
        }
        if query.group.is_some_and(|g| g != record.group) {
            continue;
        }
        if !query.text.trim().is_empty() && !matches_text(&record.haystack(), &query.text) {
            continue;
        }
        included.push(index);
    }
    let mut listing = Listing {
        in_scope,
        shown: included.len(),
        ..Listing::default()
    };
    let push = |listing: &mut Listing, row: Row, key: Option<Key>| {
        listing.rows.push(row);
        listing.keys.push(key);
    };
    let section = |listing: &mut Listing, owner: Option<WorkspaceId>, indexes: &[usize]| {
        for group in Group::ALL {
            let members: Vec<usize> = indexes
                .iter()
                .copied()
                .filter(|i| presentation.records[*i].group == group)
                .collect();
            if members.is_empty() {
                continue;
            }
            let foldable = group == Group::Finished;
            let open_group =
                !foldable || query.finished_open || query.group == Some(Group::Finished);
            let requests = members
                .iter()
                .map(|i| {
                    let r = &presentation.records[*i];
                    r.requests.len() + r.submitting
                })
                .sum();
            push(
                listing,
                Row::Group {
                    group,
                    count: members.len(),
                    requests,
                    foldable: foldable && query.group != Some(Group::Finished),
                    open: open_group,
                },
                Some(Key::Group(owner, group)),
            );
            if !open_group {
                continue;
            }
            for index in members {
                let record = &presentation.records[index];
                push(
                    listing,
                    Row::Agent {
                        session: record.session,
                    },
                    Some(Key::Agent(record.session)),
                );
                if open.contains(&record.session) {
                    for node in &record.children {
                        push(
                            listing,
                            Row::Child {
                                session: record.session,
                                node: node.node_id.clone(),
                            },
                            Some(Key::Child(record.session, node.node_id.clone())),
                        );
                    }
                }
            }
        }
    };
    if query.by_workspace {
        // Outer groups: the workspace a project is in, else the project itself; the ones with
        // the most urgent sessions first, then by name.
        let mut outer: BTreeMap<WorkspaceId, (String, Vec<usize>)> = BTreeMap::new();
        for index in &included {
            let record = &presentation.records[*index];
            let id = record.container.unwrap_or(record.workspace);
            let name = record
                .container_name
                .clone()
                .unwrap_or_else(|| record.project.clone());
            outer
                .entry(id)
                .or_insert_with(|| (name, Vec::new()))
                .1
                .push(*index);
        }
        let mut outer: Vec<(WorkspaceId, String, Vec<usize>)> = outer
            .into_iter()
            .map(|(id, (name, indexes))| (id, name, indexes))
            .collect();
        outer.sort_by(|a, b| {
            let best =
                |indexes: &[usize]| indexes.iter().map(|i| presentation.records[*i].group).min();
            best(&a.2)
                .cmp(&best(&b.2))
                .then_with(|| a.1.to_lowercase().cmp(&b.1.to_lowercase()))
                .then_with(|| a.0.cmp(&b.0))
        });
        for (id, name, indexes) in outer {
            push(
                &mut listing,
                Row::Workspace {
                    id,
                    name,
                    count: indexes.len(),
                },
                None,
            );
            section(&mut listing, Some(id), &indexes);
        }
    } else {
        section(&mut listing, None, &included);
    }
    listing
}

/// What the body of the page says when there is nothing to list.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Empty {
    /// There are rows.
    No,
    /// The first snapshot has not arrived: skeleton rows, never a false zero.
    Loading,
    /// No agent session exists anywhere: the teaching state.
    NoAgents,
    /// Sessions exist, but the scope, the search or the filter hides them all.
    NoMatch,
}

pub fn empty(presentation: &AgentPresentation, listing: &Listing) -> Empty {
    if presentation.link == Link::Loading {
        Empty::Loading
    } else if presentation.records.is_empty() {
        Empty::NoAgents
    } else if listing.shown == 0 {
        Empty::NoMatch
    } else {
        Empty::No
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sessions::presentation::*;

    fn record(title: &str, group: Group, workspace: WorkspaceId) -> AgentRecord {
        AgentRecord {
            session: SessionId::new(),
            title: title.into(),
            role: Role::Agent,
            state: match group {
                Group::NeedsYou => State::Blocked,
                Group::Working => State::Working,
                Group::Unconfirmed => State::Unconfirmed,
                Group::Idle => State::Idle,
                Group::Finished => State::Completed,
            },
            group,
            reasons: Vec::new(),
            requests: Vec::new(),
            submitting: 0,
            questions: Vec::new(),
            agent: "omp".into(),
            harness: "OMP".into(),
            model: None,
            workspace,
            container: None,
            project: "hutan".into(),
            container_name: None,
            checkout: None,
            folder: None,
            activity: None,
            detail: String::new(),
            updated_at_ms: None,
            reported_at_ms: None,
            started_at_ms: None,
            ended_at_ms: None,
            usage: None,
            transcript: None,
            provenance: Vec::new(),
            children: Vec::new(),
            subagents_live: 0,
            subagents_finished: 0,
            subagents_blocked: 0,
            live: true,
            managed: false,
            turn_active: false,
        }
    }

    fn presentation(records: Vec<AgentRecord>) -> AgentPresentation {
        AgentPresentation::new(records, Link::Online, 1)
    }

    fn titles(p: &AgentPresentation, listing: &Listing) -> Vec<String> {
        listing
            .rows
            .iter()
            .map(|row| match row {
                Row::Group { group, .. } => format!("# {}", group.label()),
                Row::Workspace { name, .. } => format!("@ {name}"),
                Row::Agent { session } => p.get(*session).unwrap().title.clone(),
                Row::Child { node, .. } => format!("  child {node}"),
            })
            .collect()
    }

    #[test]
    fn empty_groups_disappear_and_finished_is_folded_by_default() {
        let ws = WorkspaceId::new();
        let p = presentation(vec![
            record("a", Group::NeedsYou, ws),
            record("b", Group::Idle, ws),
            record("c", Group::Finished, ws),
        ]);
        let listing = list(&p, &Query::default(), &HashSet::new());
        assert_eq!(
            titles(&p, &listing),
            ["# Needs you", "a", "# Idle", "b", "# Finished"]
        );
        let open = Query {
            finished_open: true,
            ..Query::default()
        };
        let listing = list(&p, &open, &HashSet::new());
        assert_eq!(titles(&p, &listing).last().unwrap(), "c");
    }

    #[test]
    fn search_matches_title_harness_and_location_words() {
        let ws = WorkspaceId::new();
        let mut a = record("Fix dock collapse", Group::Working, ws);
        a.model = Some("Claude Sonnet".into());
        let b = record("Audit keyboard", Group::Working, ws);
        let p = presentation(vec![a, b]);
        for (text, expected) in [
            ("dock", 1),
            ("sonnet", 1),
            ("omp", 2),
            ("hutan keyboard", 1),
            ("nothing", 0),
        ] {
            let query = Query {
                text: text.into(),
                ..Query::default()
            };
            assert_eq!(list(&p, &query, &HashSet::new()).shown, expected, "{text}");
        }
    }

    #[test]
    fn keyboard_steps_skip_headings_and_clamp() {
        let ws = WorkspaceId::new();
        let p = presentation(vec![
            record("a", Group::NeedsYou, ws),
            record("b", Group::Idle, ws),
            record("c", Group::Idle, ws),
        ]);
        let listing = list(&p, &Query::default(), &HashSet::new());
        // rows: # Needs you, a, # Idle, b, c
        let first = listing.first().unwrap();
        assert_eq!(titles(&p, &listing)[first], "a");
        let next = listing.step(Some(first), 1, 1).unwrap();
        assert_eq!(titles(&p, &listing)[next], "b");
        let last = listing.last().unwrap();
        assert_eq!(titles(&p, &listing)[last], "c");
        assert_eq!(listing.step(Some(last), 1, 1), Some(last), "clamped");
        assert_eq!(listing.step(Some(first), -1, 1), Some(first), "clamped");
        assert_eq!(
            listing.step(Some(first), 1, 50),
            Some(last),
            "a page past the end"
        );
        assert_eq!(listing.step(None, 1, 1), Some(first));
        assert_eq!(listing.step(None, -1, 1), Some(last));
    }

    #[test]
    fn a_selection_by_key_survives_rows_coming_and_going() {
        let ws = WorkspaceId::new();
        let a = record("a", Group::Idle, ws);
        let b = record("b", Group::Idle, ws);
        let wanted = Key::Agent(b.session);
        let before = presentation(vec![a.clone(), b.clone()]);
        let listing = list(&before, &Query::default(), &HashSet::new());
        let at = listing.position(&wanted).unwrap();
        // A needs-you session appears above: the row index moves, the key still finds it.
        let c = record("c", Group::NeedsYou, ws);
        let after = presentation(vec![c, a, b]);
        let relisted = list(&after, &Query::default(), &HashSet::new());
        let moved = relisted.position(&wanted).unwrap();
        assert!(moved > at);
        assert_eq!(titles(&after, &relisted)[moved], "b");
    }

    #[test]
    fn children_are_listed_under_their_open_session() {
        let ws = WorkspaceId::new();
        let mut a = record("a", Group::Working, ws);
        a.children = vec![Child {
            node_id: "n1".into(),
            depth: 1,
            label: "scout".into(),
            agent_type: None,
            state: State::Working,
            blocked: false,
            finished: false,
            activity: None,
            model: None,
            started_at_ms: None,
            ended_at_ms: None,
            usage: None,
            transcript: None,
            ancestors: Vec::new(),
        }];
        let session = a.session;
        let p = presentation(vec![a]);
        let closed = list(&p, &Query::default(), &HashSet::new());
        assert_eq!(closed.rows.len(), 2);
        let opened = list(&p, &Query::default(), &HashSet::from([session]));
        assert_eq!(opened.rows.len(), 3);
        assert_eq!(opened.key(2), Some(&Key::Child(session, "n1".into())));
    }

    #[test]
    fn group_by_workspace_nests_the_same_state_groups() {
        let team = WorkspaceId::new();
        let solo = WorkspaceId::new();
        let mut a = record("a", Group::Idle, team);
        a.container = Some(team);
        a.container_name = Some("Team".into());
        let mut b = record("b", Group::NeedsYou, solo);
        b.project = "solo".into();
        let p = presentation(vec![b, a]);
        let query = Query {
            by_workspace: true,
            ..Query::default()
        };
        let listing = list(&p, &query, &HashSet::new());
        assert_eq!(
            titles(&p, &listing),
            ["@ solo", "# Needs you", "b", "@ Team", "# Idle", "a"],
            "the workspace with the more urgent session first"
        );
    }

    #[test]
    fn the_state_filter_shows_only_that_group_and_finished_is_then_unfolded() {
        let ws = WorkspaceId::new();
        let p = presentation(vec![
            record("a", Group::Unconfirmed, ws),
            record("b", Group::Finished, ws),
        ]);
        let query = Query {
            group: Some(Group::Unconfirmed),
            ..Query::default()
        };
        assert_eq!(
            titles(&p, &list(&p, &query, &HashSet::new())),
            ["# Unconfirmed", "a"]
        );
        let query = Query {
            group: Some(Group::Finished),
            ..Query::default()
        };
        assert_eq!(
            titles(&p, &list(&p, &query, &HashSet::new())),
            ["# Finished", "b"]
        );
    }

    #[test]
    fn empty_states_are_told_apart() {
        let ws = WorkspaceId::new();
        let none = presentation(Vec::new());
        assert_eq!(
            empty(&none, &list(&none, &Query::default(), &HashSet::new())),
            Empty::NoAgents
        );
        let loading = AgentPresentation::empty(Link::Loading, 0);
        assert_eq!(
            empty(
                &loading,
                &list(&loading, &Query::default(), &HashSet::new())
            ),
            Empty::Loading
        );
        let some = presentation(vec![record("a", Group::Idle, ws)]);
        let query = Query {
            text: "zzz".into(),
            ..Query::default()
        };
        assert_eq!(
            empty(&some, &list(&some, &query, &HashSet::new())),
            Empty::NoMatch
        );
        assert_eq!(
            empty(&some, &list(&some, &Query::default(), &HashSet::new())),
            Empty::No
        );
    }
}
