use serde::{Deserialize, Serialize};
use ts_rs::TS;

use super::events::Who;
use super::messages::{Kind, MessageKind};
use super::text::Block;
use super::waits::{Closed, ScreenWait};
use super::{Place, Status};
use crate::ids::{AgentId, NodeId};

#[derive(Clone, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct ChatRoom {
    pub id: String,
    pub name: String,
    pub kind: ChatRoomKind,
    pub archived: bool,
    pub members: Vec<NodeId>,
    pub outside: String,
    pub unread: u32,
    pub questions: u32,
    pub done: u32,
    #[ts(type = "number | null")]
    pub last_activity: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub last_preview: Option<String>,
}

#[derive(Clone, Serialize, Deserialize, TS)]
#[serde(tag = "kind", rename_all = "camelCase")]
#[ts(export)]
pub enum ChatRoomKind {
    Pinned,
    Agent { node: NodeId },
    Work { place: Place },
    Custom,
}

#[derive(Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct ChatRecipients {
    pub rooms: Vec<ChatRoom>,
    pub agents: Vec<ChatRecipient>,
}

#[derive(Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct ChatRecipient {
    pub node: NodeId,
    pub name: String,
    pub work: String,
    pub harness: Option<AgentId>,
    pub status: Status,
    pub task: Option<String>,
    pub modes: Vec<Kind>,
    #[ts(type = "number | null")]
    pub last_activity: Option<i64>,
}

#[derive(Serialize, Deserialize, TS)]
#[ts(export)]
pub struct SaveChatRoom {
    pub name: String,
    pub members: Vec<NodeId>,
}

#[derive(Serialize, Deserialize, TS)]
#[ts(export)]
pub struct SendChat {
    pub text: String,
    pub mode: Kind,
    #[serde(default)]
    #[ts(optional)]
    pub nodes: Option<Vec<NodeId>>,
    #[serde(default)]
    #[ts(optional)]
    pub rooms: Option<Vec<String>>,
    #[ts(optional)]
    pub room: Option<String>,
    #[serde(default)]
    #[ts(optional)]
    pub confirmed_nodes: Option<Vec<NodeId>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub attachment_nodes: Option<Vec<NodeId>>,
}

#[derive(Serialize, Deserialize, TS)]
#[ts(export)]
pub struct EditChat {
    pub text: String,
}

#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct ChatCursor {
    #[ts(type = "number")]
    pub at_ms: i64,
    pub source: ChatSource,
    #[ts(type = "number")]
    pub id: i64,
    pub part: u32,
}

#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub enum ChatSource {
    Message,
    Talk,
    Wait,
    Notice,
    Transcript,
}

#[derive(Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct ChatPage {
    pub items: Vec<ChatItem>,
    pub before: Option<ChatCursor>,
}

#[derive(Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct ChatSession {
    pub transcript: ChatTranscript,
    pub page: ChatPage,
}

#[derive(Serialize, Deserialize, TS)]
#[serde(tag = "kind", rename_all = "camelCase")]
#[ts(export)]
pub enum ChatTranscript {
    Available,
    Fallback { reason: String },
}

#[derive(Serialize, Deserialize, TS)]
#[ts(export)]
pub struct ChatItem {
    pub cursor: ChatCursor,
    #[serde(flatten)]
    #[ts(flatten)]
    pub entry: ChatEntry,
}

#[derive(Serialize, Deserialize, TS)]
#[serde(
    tag = "kind",
    rename_all = "camelCase",
    rename_all_fields = "camelCase"
)]
#[ts(export)]
pub enum ChatEntry {
    Message {
        message: ChatMessage,
    },
    Wait {
        node: Who,
        wait: ScreenWait,
        closed: Option<Closed>,
    },
    Notice {
        event: super::events::Entry,
    },
    Transcript {
        item: ChatTranscriptItem,
    },
}

#[derive(Serialize, Deserialize, TS)]
#[serde(tag = "kind", rename_all = "camelCase")]
#[ts(export)]
pub enum ChatTranscriptItem {
    User {
        text: String,
    },
    Assistant {
        text: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        #[ts(optional)]
        blocks: Option<Vec<Block>>,
    },
    ToolCall {
        id: Option<String>,
        name: String,
        summary: Option<String>,
    },
    ToolResult {
        id: Option<String>,
        summary: Option<String>,
        failed: Option<bool>,
    },
    Thinking {
        summary: Option<String>,
    },
    Status {
        text: String,
    },
}

#[derive(Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct ChatMessage {
    pub id: u32,
    pub group: Option<u32>,
    pub from: Who,
    pub origin: ChatOrigin,
    pub text: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub blocks: Option<Vec<Block>>,
    pub mode: Option<Kind>,
    pub what: Option<MessageKind>,
    pub reply_to: Option<u32>,
    pub room: Option<String>,
    pub receipts: Vec<ChatReceipt>,
    pub outside_read: bool,
    pub editable: bool,
}

#[derive(Clone, Copy, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub enum ChatOrigin {
    Human,
    Agent,
    Loom,
    Outside,
}

#[derive(Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct ChatReceipt {
    pub message: u32,
    pub to: Who,
    pub state: ChatDelivery,
}

#[derive(Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub enum ChatDelivery {
    Queued,
    Handed,
    Printed,
    Read,
    Retrying,
    Ended,
}
