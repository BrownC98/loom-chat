use std::cmp::Reverse;
use std::collections::{HashMap, HashSet};

use api::graph::Status;
use api::graph::chat::{ChatRoom, ChatRoomKind};
use api::ids::NodeId;
use gpui::App;

use super::{ChatData, Group, Rooms, group_for};

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

    pub(super) fn label(self) -> &'static str {
        match self {
            Self::All => "전체",
            Self::MyTurn => "내 차례",
            Self::Working => "일하는 중",
            Self::Unread => "안 읽음",
        }
    }

    pub(super) fn shows(self, room: &ChatRoom, index: &Index<'_>) -> bool {
        match self {
            Self::All => true,
            Self::MyTurn => room.questions > 0,
            Self::Working => working(room, &index.working_agents),
            Self::Unread => room.unread > 0,
        }
    }
}

pub(super) struct Index<'a> {
    pub(super) rooms: &'a [ChatRoom],
    pub(super) selected: Option<&'a ChatRoom>,
    pub(super) working_agents: HashSet<NodeId>,
    agents: HashMap<NodeId, &'a str>,
    work_by_agent: HashMap<NodeId, &'a str>,
}

impl<'a> Index<'a> {
    pub(super) fn new(data: &'a ChatData, cx: &'a App) -> Self {
        let mut agents = HashMap::new();
        let mut work_by_agent = HashMap::new();
        for room in data.rooms() {
            match &room.kind {
                ChatRoomKind::Agent { node } => {
                    agents.insert(*node, room.name.as_str());
                }
                ChatRoomKind::Work { .. } => {
                    for node in &room.members {
                        work_by_agent.insert(*node, room.id.as_str());
                    }
                }
                _ => {}
            }
        }
        let working_agents = if let Some(recipients) = data.recipients() {
            recipients
                .agents
                .iter()
                .filter(|agent| matches!(agent.status, Status::Working { .. }))
                .map(|agent| agent.node)
                .collect()
        } else {
            data.graph()
                .read(cx)
                .snapshot()
                .into_iter()
                .flat_map(|snapshot| &snapshot.nodes)
                .filter(|node| matches!(node.status, Status::Working { .. }))
                .map(|node| node.id)
                .collect()
        };
        Self {
            rooms: data.rooms(),
            selected: data.room(),
            working_agents,
            agents,
            work_by_agent,
        }
    }

    pub(super) fn work(&self, node: NodeId) -> Option<&'a str> {
        self.work_by_agent.get(&node).copied()
    }

    pub(super) fn matches(&self, room: &ChatRoom, query: &str) -> bool {
        query.is_empty()
            || room.name.to_lowercase().contains(query)
            || room.members.iter().any(|node| {
                self.agents
                    .get(node)
                    .is_some_and(|name| name.to_lowercase().contains(query))
            })
    }
}

pub(super) fn working(room: &ChatRoom, agents: &HashSet<NodeId>) -> bool {
    !room.archived && room.members.iter().any(|node| agents.contains(node))
}

pub(super) enum Row {
    Room(usize),
    Session(usize),
    Group {
        group: Group,
        title: String,
    },
    Sessions {
        group: Group,
        count: usize,
        open: bool,
    },
    Hidden(usize),
}

enum SessionRows {
    Folded(usize),
    Open(Vec<usize>),
}

impl SessionRows {
    fn add(&mut self, slot: usize) {
        match self {
            Self::Folded(count) => *count += 1,
            Self::Open(slots) => slots.push(slot),
        }
    }

    fn append_to(self, rows: &mut Vec<Row>, group: Group) {
        let (count, slots) = match self {
            Self::Folded(count) => (count, None),
            Self::Open(slots) => (slots.len(), Some(slots)),
        };
        rows.push(Row::Sessions {
            group,
            count,
            open: slots.is_some(),
        });
        rows.extend(slots.into_iter().flatten().map(Row::Session));
    }
}

impl Rooms {
    pub(super) fn list_rows(&self, index: &Index<'_>, query: &str) -> (Vec<Row>, usize) {
        let mut hidden = 0;
        let matching = index
            .rooms
            .iter()
            .enumerate()
            .filter(|(_, room)| !matches!(room.kind, ChatRoomKind::Pinned))
            .filter(|(_, room)| {
                let visible = self.filter.shows(room, index) && index.matches(room, query);
                hidden += usize::from(!visible);
                visible
            });
        let rows = if self.flat {
            let mut matches = matching.collect::<Vec<_>>();
            matches.sort_by_key(|(_, room)| (Reverse(room.last_activity), &room.name, &room.id));
            matches
                .into_iter()
                .map(|(slot, _)| Row::Room(slot))
                .collect()
        } else {
            self.grouped_rows(matching, index, query)
        };
        (rows, hidden)
    }

    fn grouped_rows<'a>(
        &self,
        matching: impl Iterator<Item = (usize, &'a ChatRoom)>,
        index: &Index<'a>,
        query: &str,
    ) -> Vec<Row> {
        let mut rows = Vec::new();
        let mut work_rooms = HashMap::new();
        let mut sessions = HashMap::new();
        let custom_open = !self.folded.contains(&Group::Custom);
        let auto_expand = self.filter != Filter::All || !query.is_empty();
        for (slot, room) in matching {
            match &room.kind {
                ChatRoomKind::Pinned => {}
                ChatRoomKind::Custom => {
                    if rows.is_empty() {
                        rows.push(Row::Group {
                            group: Group::Custom,
                            title: "내가 만든 방".to_owned(),
                        });
                    }
                    if custom_open {
                        rows.push(Row::Room(slot));
                    }
                }
                ChatRoomKind::Work { .. } => {
                    work_rooms.insert(room.id.as_str(), slot);
                }
                ChatRoomKind::Agent { node } => {
                    let work = index.work(*node);
                    sessions
                        .entry(work)
                        .or_insert_with(|| {
                            let group = group_for(work);
                            let open = self.sessions.get(&group).copied().unwrap_or(auto_expand);
                            if open && !self.folded.contains(&group) {
                                SessionRows::Open(Vec::new())
                            } else {
                                SessionRows::Folded(0)
                            }
                        })
                        .add(slot);
                }
            }
        }

        for work in index
            .rooms
            .iter()
            .filter(|room| matches!(room.kind, ChatRoomKind::Work { .. }))
        {
            let slot = work_rooms.get(work.id.as_str()).copied();
            let agents = sessions.remove(&Some(work.id.as_str()));
            if slot.is_none() && agents.is_none() {
                continue;
            }
            let group = Group::Work(work.id.clone());
            rows.push(Row::Group {
                group: group.clone(),
                title: work.name.clone(),
            });
            if !self.folded.contains(&group) {
                rows.extend(slot.map(Row::Room));
                if let Some(agents) = agents {
                    agents.append_to(&mut rows, group);
                }
            }
        }
        if let Some(agents) = sessions.remove(&None) {
            rows.push(Row::Group {
                group: Group::Unassigned,
                title: "작업 없는 세션".to_owned(),
            });
            if !self.folded.contains(&Group::Unassigned) {
                agents.append_to(&mut rows, Group::Unassigned);
            }
        }
        rows
    }
}
