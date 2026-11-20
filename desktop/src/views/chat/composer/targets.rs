use std::collections::{BTreeSet, HashMap};

use api::graph::Status;
use api::graph::chat::{ChatRecipient, ChatRecipients, ChatRoom, ChatRoomKind, SendChat};
use api::graph::messages::Kind;
use api::ids::NodeId;

#[derive(Clone, PartialEq, Eq)]
pub(super) enum Target {
    Node(NodeId),
    Room(String),
}

pub(super) struct Catalog<'a> {
    pub rooms: &'a [ChatRoom],
    pub agents: &'a [ChatRecipient],
    nodes: HashMap<NodeId, &'a ChatRecipient>,
    room_index: HashMap<&'a str, &'a ChatRoom>,
}

impl<'a> Catalog<'a> {
    pub fn new(data: &'a ChatRecipients) -> Self {
        Self {
            rooms: &data.rooms,
            agents: &data.agents,
            nodes: data
                .agents
                .iter()
                .map(|agent| (agent.node, agent))
                .collect(),
            room_index: data
                .rooms
                .iter()
                .map(|room| (room.id.as_str(), room))
                .collect(),
        }
    }

    pub fn node(&self, node: NodeId) -> Option<&'a ChatRecipient> {
        self.nodes.get(&node).copied()
    }

    pub fn room(&self, id: &str) -> Option<&'a ChatRoom> {
        self.room_index.get(id).copied()
    }

    pub fn default_target(&self, room: &ChatRoom) -> Option<Target> {
        if room.archived {
            return None;
        }
        match room.kind {
            ChatRoomKind::Pinned => None,
            ChatRoomKind::Agent { node } => Some(Target::Node(node)),
            ChatRoomKind::Work { .. } | ChatRoomKind::Custom => {
                let live = self.room(&room.id)?;
                Some(match live.members.first() {
                    Some(node) => Target::Node(*node),
                    None => Target::Room(room.id.clone()),
                })
            }
        }
    }

    pub fn plan(&self, selected: &[Target]) -> Plan {
        let mut plan = Plan::default();
        for target in selected {
            match target {
                Target::Node(node) if self.node(*node).is_some() => {
                    plan.nodes.insert(node.0);
                    plan.direct.insert(node.0);
                }
                Target::Room(id) => match self.room(id).filter(|room| group(room)) {
                    Some(room) => {
                        if plan.rooms.insert(id.clone()) {
                            plan.outside_rooms += usize::from(room.members.is_empty());
                            plan.nodes.extend(room.members.iter().map(|node| node.0));
                        }
                    }
                    None => plan.unavailable = true,
                },
                Target::Node(_) => plan.unavailable = true,
            }
        }
        plan.working = plan.nodes.iter().any(|&id| {
            self.node(NodeId(id))
                .is_some_and(|agent| matches!(agent.status, Status::Working { .. }))
        });
        plan.queue_only = plan.outside_rooms > 0
            || plan.nodes.iter().any(|&id| {
                self.node(NodeId(id))
                    .is_some_and(|agent| !agent.modes.contains(&Kind::Interrupt))
            });
        plan
    }
}

pub(super) fn group(room: &ChatRoom) -> bool {
    !room.archived && matches!(room.kind, ChatRoomKind::Work { .. } | ChatRoomKind::Custom)
}

#[derive(Default)]
pub(super) struct Plan {
    pub nodes: BTreeSet<u32>,
    direct: BTreeSet<u32>,
    rooms: BTreeSet<String>,
    pub outside_rooms: usize,
    pub queue_only: bool,
    pub working: bool,
    pub unavailable: bool,
}

impl Plan {
    pub fn count(&self) -> usize {
        self.nodes.len() + usize::from(self.outside_rooms > 0)
    }

    pub fn request(&self, current: &ChatRoom, text: String, mode: Kind) -> SendChat {
        let attach = group(current)
            && !self.rooms.contains(&current.id)
            && current
                .members
                .iter()
                .any(|node| self.direct.contains(&node.0));
        SendChat {
            text,
            mode,
            nodes: (!self.direct.is_empty())
                .then(|| self.direct.iter().copied().map(NodeId).collect()),
            rooms: (!self.rooms.is_empty()).then(|| self.rooms.iter().cloned().collect()),
            room: attach.then(|| current.id.clone()),
            confirmed_nodes: (mode == Kind::Interrupt)
                .then(|| self.nodes.iter().copied().map(NodeId).collect()),
            attachment_nodes: None,
        }
    }
}

pub(super) fn name<'a>(
    target: &Target,
    catalog: Option<&Catalog<'a>>,
    rooms: &'a [ChatRoom],
) -> Option<&'a str> {
    match target {
        Target::Node(node) => catalog
            .and_then(|catalog| catalog.node(*node))
            .map(|agent| agent.name.as_str())
            .or_else(|| {
                rooms.iter().find_map(|room| match room.kind {
                    ChatRoomKind::Agent { node: id } if id == *node => Some(room.name.as_str()),
                    _ => None,
                })
            }),
        Target::Room(id) => rooms
            .iter()
            .find(|room| room.id == *id)
            .map(|room| room.name.as_str()),
    }
}
