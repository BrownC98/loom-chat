use std::collections::HashMap;

use api::graph::Status;
use api::graph::chat::{ChatRoom, ChatRoomKind};
use api::ids::NodeId;
use gpui::App;

use super::ChatData;

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
            Self::Working => index.working(room),
            Self::Unread => room.unread > 0,
        }
    }
}

pub(super) struct Index<'a> {
    pub(super) rooms: &'a [ChatRoom],
    pub(super) selected: Option<&'a ChatRoom>,
    agents: HashMap<NodeId, &'a str>,
    work_by_agent: HashMap<NodeId, &'a str>,
    statuses: HashMap<NodeId, &'a Status>,
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
        let statuses = if let Some(recipients) = data.recipients() {
            recipients
                .agents
                .iter()
                .map(|agent| (agent.node, &agent.status))
                .collect()
        } else {
            data.graph()
                .read(cx)
                .snapshot()
                .into_iter()
                .flat_map(|snapshot| &snapshot.nodes)
                .map(|node| (node.id, &node.status))
                .collect()
        };
        Self {
            rooms: data.rooms(),
            selected: data.room(),
            agents,
            work_by_agent,
            statuses,
        }
    }

    fn status(&self, node: NodeId) -> Option<&Status> {
        self.statuses.get(&node).copied()
    }

    pub(super) fn working(&self, room: &ChatRoom) -> bool {
        !room.archived
            && room
                .members
                .iter()
                .any(|node| matches!(self.status(*node), Some(Status::Working { .. })))
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
