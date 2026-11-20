use std::cmp::Reverse;
use std::collections::{HashMap, HashSet};

use api::graph::chat::{ChatRoom, ChatRoomKind};
use api::graph::{NodeView, Status};
use api::ids::{NodeId, RepoId};
use api::repos::RepoView;
use gpui::App;

use super::{ChatData, Rooms, words};

#[derive(Clone, Copy, Default, PartialEq, Eq)]
pub(super) enum Filter {
    #[default]
    All,
    MyTurn,
    Working,
    Unread,
}

impl Filter {
    pub(super) const ALL: [Self; 4] = [Self::All, Self::MyTurn, Self::Working, Self::Unread];

    pub(super) fn label(self, cx: &App) -> &'static str {
        match self {
            Self::All => words(cx, "전체", "All"),
            Self::MyTurn => words(cx, "내 차례", "My turn"),
            Self::Working => words(cx, "일하는 중", "Working"),
            Self::Unread => words(cx, "안 읽음", "Unread"),
        }
    }

    pub(super) fn shows(self, room: &ChatRoom, index: &Index<'_>) -> bool {
        match self {
            Self::All => true,
            Self::MyTurn => {
                room.questions > 0
                    || matches!(
                        room.kind,
                        ChatRoomKind::Agent { .. } | ChatRoomKind::Work { .. }
                    ) && room.members.iter().any(|node| {
                        index
                            .node(*node)
                            .is_some_and(|node| matches!(node.status, Status::Human))
                    })
            }
            Self::Working => room.members.iter().any(|node| {
                index
                    .node(*node)
                    .is_some_and(|node| matches!(node.status, Status::Working { .. }))
            }),
            Self::Unread => room.unread > 0,
        }
    }
}

pub(super) struct Index<'a> {
    pub(super) rooms: &'a [ChatRoom],
    pub(super) repos: &'a [RepoView],
    pub(super) nodes: &'a [NodeView],
    node_by_id: HashMap<NodeId, &'a NodeView>,
    agents: HashMap<NodeId, &'a str>,
    work_by_agent: HashMap<NodeId, usize>,
    work_parents: HashMap<usize, usize>,
}

impl<'a> Index<'a> {
    pub(super) fn new(data: &'a ChatData, cx: &'a App) -> Self {
        let snapshot = data.graph().read(cx).snapshot();
        let repos = snapshot.map_or(&[][..], |s| s.repos.as_slice());
        let nodes = snapshot.map_or(&[][..], |s| s.nodes.as_slice());
        let mut index = Self {
            rooms: data.rooms(),
            repos,
            nodes,
            node_by_id: nodes.iter().map(|node| (node.id, node)).collect(),
            agents: HashMap::new(),
            work_by_agent: HashMap::new(),
            work_parents: HashMap::new(),
        };
        for (slot, room) in index.rooms.iter().enumerate() {
            match &room.kind {
                ChatRoomKind::Agent { node } => {
                    index.agents.insert(*node, room.name.as_str());
                }
                ChatRoomKind::Work { .. } => {
                    index
                        .work_by_agent
                        .extend(room.members.iter().map(|node| (*node, slot)));
                }
                _ => {}
            }
        }
        for node in nodes {
            let Some((work, parent)) = index
                .work(node.id)
                .zip(node.parent.and_then(|id| index.work(id)))
            else {
                continue;
            };
            if work != parent
                && !index.work_parents.contains_key(&work)
                && !std::iter::successors(Some(parent), |slot| {
                    index.work_parents.get(slot).copied()
                })
                .any(|slot| slot == work)
            {
                index.work_parents.insert(work, parent);
            }
        }
        index
    }

    pub(super) fn node(&self, node: NodeId) -> Option<&'a NodeView> {
        self.node_by_id.get(&node).copied()
    }

    pub(super) fn repo(&self, repo: RepoId) -> Option<&'a RepoView> {
        self.repos.iter().find(|entry| entry.id == repo)
    }

    pub(super) fn work(&self, node: NodeId) -> Option<usize> {
        self.work_by_agent.get(&node).copied()
    }

    pub(super) fn work_path(&self, work: usize) -> impl Iterator<Item = &ChatRoom> {
        std::iter::successors(Some(work), |slot| self.work_parents.get(slot).copied())
            .map(|slot| &self.rooms[slot])
    }

    fn member_repo(&self, node: NodeId) -> Option<RepoId> {
        if let Some(live) = self.node(node) {
            return Some(live.place.repo);
        }
        let ChatRoomKind::Work { place } = &self.rooms[self.work(node)?].kind else {
            return None;
        };
        Some(place.repo)
    }

    pub(super) fn in_workspace(&self, room: &ChatRoom, repo: Option<RepoId>) -> bool {
        let Some(repo) = repo else { return true };
        match &room.kind {
            ChatRoomKind::Pinned => true,
            ChatRoomKind::Work { place } => place.repo == repo,
            ChatRoomKind::Agent { node } => self.member_repo(*node) == Some(repo),
            ChatRoomKind::Custom => room
                .members
                .iter()
                .any(|&node| self.member_repo(node) == Some(repo)),
        }
    }

    pub(super) fn work_title(&self, room: &ChatRoom, cx: &App) -> String {
        let title = super::room_title(room, |id| self.repo(id), cx);
        if let ChatRoomKind::Work { place } = &room.kind
            && self.repos.len() > 1
            && let Some(repo) = self.repo(place.repo)
            && repo.git
        {
            format!("{} / {title}", repo.name)
        } else {
            title.to_owned()
        }
    }

    pub(super) fn matches(&self, room: &ChatRoom, query: &str, cx: &App) -> bool {
        let contains = |text: &str| text.to_lowercase().contains(query);
        query.is_empty()
            || contains(&room.name)
            || contains(&self.work_title(room, cx))
            || matches!(&room.kind, ChatRoomKind::Work { place }
                if self.repo(place.repo).is_some_and(|repo| contains(&repo.name)))
            || room.members.iter().any(|&node| {
                self.agents.get(&node).is_some_and(|name| contains(name))
                    || self
                        .member_repo(node)
                        .and_then(|id| self.repo(id))
                        .is_some_and(|repo| contains(&repo.name))
            })
    }
}

pub(super) enum Row {
    Room(usize),
    Work(usize, Vec<bool>),
    Agent(usize, Vec<bool>),
    Custom,
    Worktrees,
    Unassigned,
    Hidden(usize),
}

impl Rooms {
    pub(super) fn list_rows(&self, index: &Index<'_>, query: &str, cx: &App) -> (Vec<Row>, usize) {
        let all = index.rooms.iter().enumerate().filter(|(_, room)| {
            !matches!(room.kind, ChatRoomKind::Pinned) && index.in_workspace(room, self.workspace)
        });
        let mut hidden = 0;
        let mut matches: Vec<_> = all
            .filter(|(_, room)| {
                let visible = self.filter.shows(room, index) && index.matches(room, query, cx);
                hidden += usize::from(!visible);
                visible
            })
            .collect();
        let rows = if self.flat {
            matches.sort_by_key(|(_, room)| (Reverse(room.last_activity), &room.name, &room.id));
            matches
                .into_iter()
                .map(|(slot, room)| match room.kind {
                    ChatRoomKind::Work { .. } => Row::Work(slot, Vec::new()),
                    ChatRoomKind::Agent { .. } => Row::Agent(slot, Vec::new()),
                    _ => Row::Room(slot),
                })
                .collect()
        } else {
            self.grouped_rows(matches, index)
        };
        (rows, hidden)
    }

    fn grouped_rows(&self, matching: Vec<(usize, &ChatRoom)>, index: &Index<'_>) -> Vec<Row> {
        let mut rows = Vec::new();
        let mut work_rooms = HashSet::new();
        let mut sessions = HashMap::<Option<usize>, Vec<(NodeId, usize)>>::new();
        for (slot, room) in matching {
            match &room.kind {
                ChatRoomKind::Pinned => {}
                ChatRoomKind::Custom => {
                    if rows.is_empty() {
                        rows.push(Row::Custom);
                    }
                    if !self.custom_folded {
                        rows.push(Row::Room(slot));
                    }
                }
                ChatRoomKind::Work { .. } => {
                    work_rooms.insert(slot);
                }
                ChatRoomKind::Agent { node } => {
                    let work = index.work(*node);
                    work_rooms.extend(work);
                    sessions.entry(work).or_default().push((*node, slot));
                }
            }
        }
        if !work_rooms.is_empty() {
            rows.push(Row::Worktrees);
        }
        let mut work_children = HashMap::<Option<usize>, Vec<usize>>::new();
        for slot in 0..index.rooms.len() {
            if work_rooms.contains(&slot) {
                let parent = index
                    .work_parents
                    .get(&slot)
                    .copied()
                    .filter(|parent| work_rooms.contains(parent));
                work_children.entry(parent).or_default().push(slot);
            }
        }
        let mut branches = Vec::new();
        push_children(&mut branches, work_children.remove(&None), &[], false);
        while let Some((slot, guides)) = branches.pop() {
            let path = &guides[1..];
            rows.push(Row::Work(slot, path.to_vec()));
            let children = work_children.remove(&Some(slot));
            if !self.folded.contains(&index.rooms[slot].id) {
                if let Some(agents) = sessions.remove(&Some(slot)) {
                    append_agents(&mut rows, index, agents, path, children.is_some());
                }
                push_children(&mut branches, children, &guides, false);
            }
        }
        if let Some(agents) = sessions.remove(&None) {
            rows.push(Row::Unassigned);
            append_agents(&mut rows, index, agents, &[], false);
        }
        rows
    }
}

fn append_agents(
    rows: &mut Vec<Row>,
    index: &Index<'_>,
    agents: Vec<(NodeId, usize)>,
    guides: &[bool],
    followed: bool,
) {
    let visible: HashSet<_> = agents.iter().map(|(node, _)| *node).collect();
    let mut children = HashMap::<Option<NodeId>, Vec<(NodeId, usize)>>::new();
    for (node, slot) in agents {
        let parent = index
            .node(node)
            .and_then(|node| node.parent)
            .filter(|parent| visible.contains(parent));
        children.entry(parent).or_default().push((node, slot));
    }
    let mut pending = Vec::new();
    push_children(&mut pending, children.remove(&None), guides, followed);
    while let Some(((node, slot), guides)) = pending.pop() {
        push_children(&mut pending, children.remove(&Some(node)), &guides, false);
        rows.push(Row::Agent(slot, guides));
    }
}

fn push_children<T>(
    pending: &mut Vec<(T, Vec<bool>)>,
    children: Option<Vec<T>>,
    guides: &[bool],
    followed: bool,
) {
    if let Some(children) = children {
        let count = children.len();
        pending.extend(
            children
                .into_iter()
                .enumerate()
                .rev()
                .map(|(index, child)| {
                    let mut guides = guides.to_vec();
                    guides.push(followed || index + 1 < count);
                    (child, guides)
                }),
        );
    }
}
