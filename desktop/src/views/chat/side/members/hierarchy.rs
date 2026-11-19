use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};
use std::hash::Hash;

use api::graph::chat::{ChatEntry, ChatMessage, ChatOrigin, ChatRecipient, ChatRoom, ChatRoomKind};
use api::graph::{NodeView, Place, Status};
use api::ids::{NodeId, RepoId};
use api::repos::RepoView;
use gpui::App;

use super::super::super::data::ChatData;

#[derive(Clone, PartialEq, Eq)]
pub(in crate::views::chat::side) enum MemberKey {
    Local(NodeId),
    Outside(String),
}

pub(in crate::views::chat::side) struct Member<'a> {
    pub key: MemberKey,
    pub name: &'a str,
    pub harness: Option<&'a str>,
    pub session: Option<&'a ChatRoom>,
    pub node: Option<&'a NodeView>,
    pub recipient: Option<&'a ChatRecipient>,
}

impl Member<'_> {
    pub fn status(&self) -> Option<&Status> {
        self.node
            .map(|node| &node.status)
            .or_else(|| self.recipient.map(|agent| &agent.status))
    }

    pub fn terminal(&self) -> Option<NodeId> {
        match self.key {
            MemberKey::Local(node)
                if self.session.is_none_or(|room| !room.archived)
                    && !matches!(
                        self.status(),
                        Some(Status::Stopped { .. } | Status::SetupFailed { .. })
                    ) =>
            {
                Some(node)
            }
            _ => None,
        }
    }
}

pub(super) enum Entry<'a> {
    Member(Member<'a>),
    Above(&'a NodeView),
    Work(&'a ChatRoom),
    Workspace {
        id: RepoId,
        rooms: usize,
        members: usize,
    },
    Unassigned,
}

type WorkKey<'a> = (RepoId, &'a str);

#[derive(Clone, Copy, PartialEq, Eq, Hash)]
enum Branch<'a> {
    Node(NodeId),
    Work(WorkKey<'a>),
}

pub(super) struct Index<'a> {
    pub nodes: HashMap<NodeId, &'a NodeView>,
    pub repos: HashMap<RepoId, &'a RepoView>,
    pub sessions: HashMap<NodeId, &'a ChatRoom>,
    pub messages: HashMap<NodeId, (&'a ChatMessage, i64)>,
    works: HashMap<WorkKey<'a>, &'a ChatRoom>,
    recipients: HashMap<NodeId, &'a ChatRecipient>,
    outside: BTreeSet<&'a str>,
}

impl<'a> Index<'a> {
    pub(super) fn new(data: &'a ChatData, cx: &'a App) -> Self {
        let snapshot = data.graph().read(cx).snapshot();
        let mut index = Self {
            nodes: snapshot
                .into_iter()
                .flat_map(|snapshot| &snapshot.nodes)
                .map(|node| (node.id, node))
                .collect(),
            repos: snapshot
                .into_iter()
                .flat_map(|snapshot| &snapshot.repos)
                .map(|repo| (repo.id, repo))
                .collect(),
            recipients: data
                .recipients()
                .into_iter()
                .flat_map(|recipients| &recipients.agents)
                .map(|agent| (agent.node, agent))
                .collect(),
            sessions: HashMap::new(),
            works: HashMap::new(),
            messages: HashMap::new(),
            outside: BTreeSet::new(),
        };
        for room in data.rooms() {
            match &room.kind {
                ChatRoomKind::Agent { node } => {
                    index.sessions.insert(*node, room);
                }
                ChatRoomKind::Work { place } => {
                    index.works.insert(work_key(place), room);
                }
                _ => {}
            }
        }
        for item in data.items().iter().rev() {
            if let ChatEntry::Message { message } = &item.entry {
                if message.origin == ChatOrigin::Outside {
                    index.outside.insert(&message.from.name);
                }
                if let Some(id) = message.from.id
                    && message.what.as_ref().is_none_or(|kind| !kind.by_loom())
                {
                    index
                        .messages
                        .entry(id)
                        .or_insert((message, item.cursor.at_ms));
                }
            }
        }
        index
    }

    pub(super) fn work_at(&self, place: &Place) -> Option<&'a ChatRoom> {
        self.works.get(&work_key(place)).copied()
    }

    pub(super) fn state(&self, id: NodeId) -> Option<&Status> {
        self.nodes
            .get(&id)
            .map(|node| &node.status)
            .or_else(|| self.recipients.get(&id).map(|agent| &agent.status))
    }

    fn local(&self, id: NodeId) -> Option<Member<'a>> {
        let node = self.nodes.get(&id).copied();
        let recipient = self.recipients.get(&id).copied();
        let session = self.sessions.get(&id).copied();
        let name = node
            .map(|node| node.name.as_str())
            .or_else(|| recipient.map(|agent| agent.name.as_str()))
            .or_else(|| session.map(|room| room.name.as_str()))?;
        Some(Member {
            key: MemberKey::Local(id),
            name,
            harness: node
                .and_then(|node| node.agent.as_ref())
                .or_else(|| recipient.and_then(|agent| agent.harness.as_ref()))
                .map(|id| id.0.as_str()),
            session,
            node,
            recipient,
        })
    }

    fn outside(&self) -> impl Iterator<Item = Member<'a>> + '_ {
        self.outside.iter().map(|&name| Member {
            key: MemberKey::Outside(name.to_owned()),
            name,
            harness: None,
            session: None,
            node: None,
            recipient: None,
        })
    }

    pub(super) fn members(&self, room: &ChatRoom) -> Vec<Member<'a>> {
        room.members
            .iter()
            .filter_map(|&node| self.local(node))
            .chain(self.outside())
            .collect()
    }

    fn parent(&self, id: NodeId) -> Option<NodeId> {
        self.nodes.get(&id)?.parent
    }

    pub(super) fn room_rows(&self, room: &ChatRoom) -> Vec<(Entry<'a>, Vec<bool>)> {
        let mut rows = match &room.kind {
            ChatRoomKind::Pinned => self.workspace_rows(room),
            ChatRoomKind::Work { place } => self.work_rows(room, place),
            _ => room
                .members
                .iter()
                .filter_map(|&node| self.local(node))
                .map(|member| (Entry::Member(member), Vec::new()))
                .collect(),
        };
        rows.extend(
            self.outside()
                .map(|member| (Entry::Member(member), Vec::new())),
        );
        rows
    }

    fn work_rows(&self, room: &ChatRoom, place: &Place) -> Vec<(Entry<'a>, Vec<bool>)> {
        let members: HashSet<_> = room.members.iter().copied().collect();
        let mut above = HashSet::new();
        let mut rows: Vec<_> = room
            .members
            .iter()
            .filter_map(|&id| self.parent(id).filter(|id| !members.contains(id)))
            .filter_map(|id| self.nodes.get(&id))
            .filter(|node| node.place != *place && above.insert(node.id))
            .map(|node| (Entry::Above(node), Vec::new()))
            .collect();

        let mut children = HashMap::new();
        let mut nodes: Vec<_> = self.nodes.values().copied().collect();
        nodes.sort_by_key(|node| node.id.0);
        for node in nodes {
            if let Some(parent) = node.parent.filter(|id| members.contains(id))
                && node.place != *place
                && self.works.contains_key(&work_key(&node.place))
            {
                children.entry(work_key(&node.place)).or_insert(parent);
            }
        }
        let mut work_ids: Vec<_> = children.keys().copied().collect();
        work_ids.sort_by_key(|id| (&self.works[id].name, &self.works[id].id));
        let branches = room
            .members
            .iter()
            .copied()
            .map(Branch::Node)
            .chain(work_ids.into_iter().map(Branch::Work));
        rows.extend(
            tree_order(branches, |branch| match branch {
                Branch::Node(node) => self
                    .parent(node)
                    .filter(|parent| members.contains(parent))
                    .map(Branch::Node),
                Branch::Work(room) => children.get(&room).copied().map(Branch::Node),
            })
            .into_iter()
            .filter_map(|(branch, guides)| {
                let row = match branch {
                    Branch::Node(node) => Entry::Member(self.local(node)?),
                    Branch::Work(key) => Entry::Work(self.works[&key]),
                };
                Some((row, guides))
            }),
        );
        rows
    }

    fn workspace_rows(&self, pinned: &ChatRoom) -> Vec<(Entry<'a>, Vec<bool>)> {
        let mut groups = BTreeMap::<u32, Vec<WorkKey>>::new();
        groups.extend(self.repos.keys().map(|id| (id.0, Vec::new())));
        let mut assigned = HashSet::new();
        for (&(repo, path), room) in &self.works {
            groups.entry(repo.0).or_default().push((repo, path));
            assigned.extend(room.members.iter().copied());
        }

        let mut nodes: Vec<_> = self.nodes.keys().copied().collect();
        nodes.sort_by_key(|node| node.0);
        let mut parents = HashMap::new();
        for (id, _) in tree_order(nodes, |id| self.parent(id)) {
            let node = self.nodes[&id];
            let key = work_key(&node.place);
            if self.works.contains_key(&key) {
                parents.entry(key).or_insert_with(|| {
                    let parent = work_key(&self.nodes.get(&node.parent?)?.place);
                    (parent.0 == key.0 && parent != key).then_some(parent)
                });
            }
        }

        let mut rows = Vec::new();
        for (id, mut rooms) in groups {
            let id = RepoId(id);
            let original = self.repos.get(&id).map(|repo| repo.path.as_str());
            rooms.sort_by_key(|key| (Some(key.1) != original, &self.works[key].name, key.1));
            rows.push((
                Entry::Workspace {
                    id,
                    rooms: rooms.len(),
                    members: rooms.iter().map(|key| self.works[key].members.len()).sum(),
                },
                Vec::new(),
            ));
            rows.extend(
                tree_order(rooms, |key| parents.get(&key).copied().flatten())
                    .into_iter()
                    .map(|(key, guides)| (Entry::Work(self.works[&key]), guides)),
            );
        }
        let unassigned: Vec<_> = pinned
            .members
            .iter()
            .filter(|id| !assigned.contains(id))
            .filter_map(|&id| self.local(id))
            .collect();
        if !unassigned.is_empty() {
            rows.push((Entry::Unassigned, Vec::new()));
            rows.extend(
                unassigned
                    .into_iter()
                    .map(|member| (Entry::Member(member), Vec::new())),
            );
        }
        rows
    }
}

fn work_key(place: &Place) -> WorkKey<'_> {
    (place.repo, &place.worktree)
}

fn tree_order<T: Copy + Eq + Hash>(
    keys: impl IntoIterator<Item = T>,
    parent: impl Fn(T) -> Option<T>,
) -> Vec<(T, Vec<bool>)> {
    let ordered: Vec<_> = keys.into_iter().collect();
    let known: HashSet<_> = ordered.iter().copied().collect();
    let mut children = HashMap::<Option<T>, Vec<T>>::new();
    for &key in &ordered {
        let parent = parent(key).filter(|parent| known.contains(parent));
        children.entry(parent).or_default().push(key);
    }
    let mut todo = Vec::new();
    let mut seen = HashSet::new();
    let mut rows = Vec::new();
    for key in children
        .remove(&None)
        .unwrap_or_default()
        .into_iter()
        .chain(ordered)
    {
        todo.push((key, vec![false]));
        while let Some((key, path)) = todo.pop() {
            if seen.insert(key) {
                if let Some(branches) = children.remove(&Some(key)) {
                    let len = branches.len();
                    for (i, branch) in branches.into_iter().enumerate().rev() {
                        let mut path = path.clone();
                        path.push(i + 1 < len);
                        todo.push((branch, path));
                    }
                }
                rows.push((key, path));
            }
        }
    }
    rows
}
