use std::collections::{BTreeMap, BTreeSet};

use axum::http::StatusCode;

use api::graph::chat::*;
use api::graph::docs::PostOutside;

use super::*;
use crate::graph::{ChatInterrupt, ChatLaunch};
use crate::place::{self, Machine};
use crate::protocol::BATCH_SEPARATOR;
use crate::transcript::{self, Fallback};

const DEFAULT_PAGE: usize = 60;
const MAX_PAGE: usize = 200;

pub(super) fn routes() -> Router<Arc<App>> {
    Router::new()
        .route("/api/chat/rooms", get(list_rooms).post(create_room))
        .route(
            "/api/chat/rooms/{room}",
            put(update_room).delete(archive_room),
        )
        .route("/api/chat/recipients", get(recipients))
        .route("/api/chat/rooms/{room}/stream", get(room_stream))
        .route("/api/chat/rooms/{room}/session", get(session))
        .route("/api/chat/messages", post(send))
        .route(
            "/api/chat/messages/{id}",
            axum::routing::patch(edit).delete(withdraw),
        )
}

pub(super) fn outside_routes() -> Router<Arc<App>> {
    Router::new().route(
        "/api/chat/rooms/{room}/outside/{token}",
        get(outside_stream)
            .head(|| async { StatusCode::METHOD_NOT_ALLOWED })
            .post(post_outside),
    )
}

#[derive(Deserialize)]
struct PageQuery {
    before: Option<String>,
    limit: Option<usize>,
}

impl PageQuery {
    fn cursor(&self) -> Result<Option<ChatCursor>, Refusal> {
        self.before
            .as_deref()
            .map(serde_json::from_str)
            .transpose()
            .map_err(|_| Refusal::BadSetting("채팅의 before 값이 틀렸습니다.".into()))
    }

    fn limit(&self) -> usize {
        self.limit.unwrap_or(DEFAULT_PAGE).clamp(1, MAX_PAGE)
    }
}

async fn list_rooms(State(app): State<Arc<App>>) -> Result<Json<Vec<ChatRoom>>, Problem> {
    Ok(Json(app.graph.chat_rooms()?))
}

async fn recipients(State(app): State<Arc<App>>) -> Result<Json<ChatRecipients>, Problem> {
    Ok(Json(app.graph.chat_recipients(|node| app.tools.of(node))?))
}

async fn create_room(
    State(app): State<Arc<App>>,
    Json(req): Json<SaveChatRoom>,
) -> Result<Json<ChatRoom>, Problem> {
    Ok(Json(app.graph.create_chat_room(req)?))
}

async fn update_room(
    State(app): State<Arc<App>>,
    Path(room): Path<String>,
    Json(req): Json<SaveChatRoom>,
) -> Result<Json<ChatRoom>, Problem> {
    Ok(Json(app.graph.update_chat_room(&room, req)?))
}

async fn archive_room(
    State(app): State<Arc<App>>,
    Path(room): Path<String>,
) -> Result<Json<()>, Problem> {
    app.graph.archive_chat_room(&room)?;
    Ok(Json(()))
}

async fn room_stream(
    State(app): State<Arc<App>>,
    Path(room): Path<String>,
    Query(page): Query<PageQuery>,
) -> Result<Json<ChatPage>, Problem> {
    Ok(Json(app.graph.chat_stream(
        &room,
        page.cursor()?,
        page.limit(),
    )?))
}

async fn outside_stream(
    State(app): State<Arc<App>>,
    Path((room, token)): Path<(String, String)>,
    Query(page): Query<PageQuery>,
) -> Result<impl IntoResponse, Problem> {
    let stream = app
        .graph
        .outside_chat_stream(&room, &token, page.cursor()?, page.limit())?;
    Ok((
        [(axum::http::header::CACHE_CONTROL, "no-store")],
        Json(stream),
    ))
}

async fn post_outside(
    State(app): State<Arc<App>>,
    Path((room, token)): Path<(String, String)>,
    Json(req): Json<PostOutside>,
) -> Result<Json<ChatMessage>, Problem> {
    Ok(Json(app.graph.post_outside_chat(&room, &token, req)?))
}

async fn send(
    State(app): State<Arc<App>>,
    Json(req): Json<SendChat>,
) -> Result<Json<ChatMessage>, Problem> {
    let mut stopped = Vec::new();
    let message = app.graph.send_chat(
        req,
        |ChatInterrupt {
             node,
             worktree,
             shells,
         }| {
            if !shells.is_empty() {
                let host = place::of(std::path::Path::new(&worktree)).host();
                if host.is_some() || app.host.stop_guarded(node, &shells) {
                    stopped.push((node, host, shells));
                }
            }
        },
    )?;
    for (node, host, shells) in stopped {
        match host {
            Some(host) => app.end_guarded_inside(node, host, shells),
            None => app.release_guarded(node, &shells),
        }
    }
    Ok(Json(message))
}

async fn edit(
    State(app): State<Arc<App>>,
    Path(id): Path<u32>,
    Json(req): Json<EditChat>,
) -> Result<Json<ChatMessage>, Problem> {
    Ok(Json(app.graph.edit_chat(id, &req.text)?))
}

async fn withdraw(State(app): State<Arc<App>>, Path(id): Path<u32>) -> Result<Json<()>, Problem> {
    app.graph.withdraw_chat(id)?;
    Ok(Json(()))
}

async fn session(
    State(app): State<Arc<App>>,
    Path(room): Path<String>,
    Query(page): Query<PageQuery>,
) -> Result<Json<ChatSession>, Problem> {
    let before = page.cursor()?;
    let limit = page.limit();
    let Some(launch) = app.graph.chat_launch(&room)? else {
        return Ok(Json(ChatSession {
            transcript: fallback("종료한 노드의 세션 기록 경로가 없습니다."),
            page: app.graph.chat_stream(&room, before, limit)?,
        }));
    };
    let (launch, transcript) = transcript_page(&app, launch, before, limit).await?;
    let (mut page, same) = app.graph.chat_session_page(&room, before, limit, &launch)?;
    let transcript = match transcript {
        _ if !same => fallback("세션 기록을 읽는 동안 실행이 바뀌었습니다."),
        Err(reason) => fallback(reason.reason()),
        Ok(mut items) => {
            for item in page.items {
                items.insert(item.cursor, item.entry);
            }
            let more = items.len() > limit || page.before.is_some();
            while items.len() > limit {
                items.pop_first();
            }
            page.items = items
                .into_iter()
                .map(|(cursor, entry)| ChatItem { cursor, entry })
                .collect();
            page.before = page.items.first().filter(|_| more).map(|item| item.cursor);
            ChatTranscript::Available
        }
    };
    Ok(Json(ChatSession { transcript, page }))
}

type TranscriptPage = BTreeMap<ChatCursor, ChatEntry>;

async fn transcript_page(
    app: &App,
    launch: ChatLaunch,
    before: Option<ChatCursor>,
    limit: usize,
) -> Result<(ChatLaunch, Result<TranscriptPage, Fallback>), Problem> {
    let Some(profile) = launch.agent.as_ref().and_then(profile::profile_of) else {
        return Ok((launch, Err(Fallback::Unsupported)));
    };
    let cwd = std::path::Path::new(&launch.worktree);
    let site = match place::of(cwd) {
        Machine::Pc => app.pc_site(),
        Machine::Wsl(_) => match app.site_at(cwd).await {
            Ok(site) => site,
            Err(_) => return Ok((launch, Err(Fallback::Unreadable))),
        },
        Machine::Ssh(_) => return Ok((launch, Err(Fallback::Unsupported))),
    };
    let graph = app.graph.clone();
    tokio::task::spawn_blocking(move || {
        let result = profile::transcript_source(
            profile,
            &site,
            launch.session.as_ref(),
            launch.account.as_ref(),
        )
        .and_then(|source| read_transcript(&graph, launch.node, &source, before, limit));
        (launch, result)
    })
    .await
    .map_err(internal)
}

fn read_transcript(
    graph: &Graph,
    node: NodeId,
    source: &transcript::Source,
    before: Option<ChatCursor>,
    limit: usize,
) -> Result<TranscriptPage, Fallback> {
    let mut found = BTreeMap::new();
    let mut after = None;
    let mut at_ms = 0;
    let mut previous_line = 0;
    let mut part = 0;
    loop {
        let transcript::Batch {
            items,
            cursor,
            more,
            reset,
        } = source.read(after.as_ref())?;
        if reset {
            return Err(Fallback::Unreadable);
        }
        let messages: BTreeSet<_> = items
            .iter()
            .filter_map(|item| match &item.kind {
                transcript::Kind::User(text) => Some(text.lines().filter_map(reference)),
                _ => None,
            })
            .flatten()
            .collect();
        let deliveries = if messages.is_empty() {
            Vec::new()
        } else {
            graph
                .chat_delivery_texts(node, &messages)
                .map_err(|_| Fallback::Unreadable)?
        };
        for item in items {
            if item.line == previous_line {
                part += 1;
            } else {
                part = 0;
                previous_line = item.line;
            }
            if let Some(at) = item
                .timestamp
                .as_deref()
                .and_then(|at| at.parse::<jiff::Timestamp>().ok())
            {
                at_ms = at.as_millisecond();
            }
            let cursor = ChatCursor {
                at_ms,
                source: ChatSource::Transcript,
                id: item.line as i64,
                part,
            };
            if before.is_some_and(|before| cursor >= before)
                || (found.len() > limit
                    && found
                        .first_key_value()
                        .is_some_and(|(oldest, _)| cursor < *oldest))
            {
                continue;
            }
            if let transcript::Kind::User(text) = &item.kind
                && !deliveries.is_empty()
                && is_delivery(text, &deliveries)
            {
                continue;
            }
            found.insert(
                cursor,
                ChatEntry::Transcript {
                    item: transcript_item(item.kind),
                },
            );
            if found.len() > limit + 1 {
                found.pop_first();
            }
        }
        if !more {
            return Ok(found);
        }
        after = Some(cursor);
    }
}

fn reference(line: &str) -> Option<u32> {
    let (id, _) = line
        .strip_prefix("loom message #")?
        .split_once(" (from: ")?;
    id.parse().ok()
}

fn is_delivery(mut text: &str, deliveries: &[String]) -> bool {
    loop {
        let Some(tail) = deliveries
            .iter()
            .filter_map(|delivery| text.strip_prefix(delivery))
            .filter(|tail| tail.is_empty() || tail.starts_with(BATCH_SEPARATOR))
            .min_by_key(|tail| tail.len())
        else {
            return false;
        };
        if tail.is_empty() {
            return true;
        }
        text = &tail[BATCH_SEPARATOR.len()..];
    }
}

fn transcript_item(kind: transcript::Kind) -> ChatTranscriptItem {
    use transcript::Kind;
    match kind {
        Kind::User(text) => ChatTranscriptItem::User { text },
        Kind::Assistant(text) => ChatTranscriptItem::Assistant { text },
        Kind::ToolCall { id, name, summary } => ChatTranscriptItem::ToolCall { id, name, summary },
        Kind::ToolResult {
            id,
            summary,
            failed,
        } => ChatTranscriptItem::ToolResult {
            id,
            summary,
            failed,
        },
        Kind::Thinking(summary) => ChatTranscriptItem::Thinking { summary },
        Kind::Status(status) => ChatTranscriptItem::Status {
            text: match status {
                transcript::Status::TurnStarted => "턴을 시작했습니다.".into(),
                transcript::Status::TurnEnded => "턴이 끝났습니다.".into(),
                transcript::Status::TurnStopped => "턴이 멈췄습니다.".into(),
                transcript::Status::Notice(text) => text,
            },
        },
    }
}

fn fallback(reason: &str) -> ChatTranscript {
    ChatTranscript::Fallback {
        reason: reason.into(),
    }
}
