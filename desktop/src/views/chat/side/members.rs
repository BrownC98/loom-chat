use std::collections::{BTreeSet, HashMap};

use api::graph::Status;
use api::graph::chat::{ChatEntry, ChatOrigin, ChatRoom, ChatRoomKind};
use api::ids::NodeId;
use gpui::{
    App, ClipboardItem, Context, Div, FontWeight, InteractiveElement, IntoElement, ParentElement,
    Role, StatefulInteractiveElement, Styled, div,
};

use super::{SideEvent, SideSlot, room_waiting, scroller, words};
use crate::fonts;
use crate::scale::px;
use crate::strings::Lang;
use crate::views::icons::{Lucide, agent_mark, icon};
use crate::views::status;
use crate::views::tooltip::tip;
use crate::views::ui::{self, GAP, SMALL};
use crate::views::widgets::btn;

#[derive(Clone, PartialEq, Eq)]
pub(super) enum MemberKey {
    Local(NodeId),
    Outside(String),
}

struct Member<'a> {
    key: MemberKey,
    name: &'a str,
    harness: Option<&'a str>,
    status: Option<&'a Status>,
    session: Option<&'a ChatRoom>,
}

impl Member<'_> {
    fn terminal(&self) -> Option<NodeId> {
        match self.key {
            MemberKey::Local(node)
                if self.session.is_none_or(|room| !room.archived)
                    && !matches!(
                        self.status,
                        Some(Status::Stopped { .. } | Status::SetupFailed { .. })
                    ) =>
            {
                Some(node)
            }
            _ => None,
        }
    }

    fn details(&self, cx: &App) -> Div {
        let c = ui::colors(cx);
        let lang = *cx.global::<Lang>();
        let line = div()
            .flex()
            .flex_wrap()
            .items_center()
            .gap(px(GAP))
            .text_size(px(SMALL))
            .text_color(c.ink_2);
        if matches!(self.key, MemberKey::Outside(_)) {
            return line
                .child(words(cx, "외부 에이전트", "Outside agent"))
                .child(words(cx, "터미널 없음", "No terminal"));
        }
        let archived = self.session.is_some_and(|room| room.archived);
        let chip = if archived {
            Some(status::Chip {
                text: words(cx, "종료", "Ended").to_owned(),
                tone: status::Tone::Waiting,
            })
        } else {
            self.status.and_then(|state| match state {
                Status::Human => Some(status::Chip {
                    text: words(cx, "답을 기다리는 중", "Waiting for your reply").to_owned(),
                    tone: status::Tone::Ask,
                }),
                state => status::chip(state, lang),
            })
        };
        line.children(self.harness.map(|name| name.to_owned()))
            .children(chip.map(|chip| status::pill(chip.text, chip.tone, 1.0, &c)))
    }
}

impl SideSlot {
    fn members<'a>(&'a self, room: &'a ChatRoom, cx: &'a App) -> Vec<Member<'a>> {
        let data = self.data.read(cx);
        let graph = data.graph().read(cx);
        let nodes: HashMap<_, _> = graph
            .snapshot()
            .into_iter()
            .flat_map(|snapshot| &snapshot.nodes)
            .map(|node| (node.id, node))
            .collect();
        let recipients: HashMap<_, _> = data
            .recipients()
            .into_iter()
            .flat_map(|recipients| &recipients.agents)
            .map(|agent| (agent.node, agent))
            .collect();
        let sessions: HashMap<_, _> = data
            .rooms()
            .iter()
            .filter_map(|room| match room.kind {
                ChatRoomKind::Agent { node } => Some((node, room)),
                _ => None,
            })
            .collect();
        let local = room.members.iter().filter_map(|&id| {
            let node = nodes.get(&id).copied();
            let recipient = recipients.get(&id).copied();
            let session = sessions.get(&id).copied();
            let name = recipient
                .map(|agent| agent.name.as_str())
                .or_else(|| node.map(|node| node.name.as_str()))
                .or_else(|| session.map(|room| room.name.as_str()))?;
            Some(Member {
                key: MemberKey::Local(id),
                name,
                harness: recipient
                    .and_then(|agent| agent.harness.as_ref())
                    .or_else(|| node.and_then(|node| node.agent.as_ref()))
                    .map(|id| id.0.as_str()),
                status: recipient
                    .map(|agent| &agent.status)
                    .or_else(|| node.map(|node| &node.status)),
                session,
            })
        });
        let outside: BTreeSet<_> = data
            .items()
            .iter()
            .filter_map(|item| match &item.entry {
                ChatEntry::Message { message } if message.origin == ChatOrigin::Outside => {
                    Some(message.from.name.as_str())
                }
                _ => None,
            })
            .collect();
        local
            .chain(outside.into_iter().map(|name| Member {
                key: MemberKey::Outside(name.to_owned()),
                name,
                harness: None,
                status: None,
                session: None,
            }))
            .collect()
    }

    pub(super) fn member_panel(&self, room: Option<&ChatRoom>, cx: &Context<Self>) -> Div {
        let c = ui::colors(cx);
        let data = self.data.read(cx);
        let mut body = div().flex().flex_col().px(px(14.0)).py(px(10.0));
        let Some(room) = room else {
            return body.child(room_waiting(cx, None));
        };
        let members = self.members(room, cx);
        let entity = cx.entity().downgrade();
        let rows: Vec<_> = members
            .into_iter()
            .enumerate()
            .map(|(index, member)| {
                let action = member.session.map(|session| {
                    let entity = entity.clone();
                    let room = session.id.clone();
                    btn::button(("chat-member-room", index), cx)
                        .role(Role::Link)
                        .aria_label(format!(
                            "{} · {}",
                            member.name,
                            words(cx, "1:1 세션", "1:1 session")
                        ))
                        .flex_none()
                        .child(words(cx, "1:1 세션", "1:1 session"))
                        .on_click(move |_, _, cx| {
                            let _ = entity.update(cx, |_, cx| {
                                cx.emit(SideEvent::OpenRoom(room.clone()));
                            });
                        })
                });
                let mark = match member.key {
                    MemberKey::Local(_) => {
                        agent_mark(member.harness, member.name, px(17.0), c.ink_2)
                    }
                    MemberKey::Outside(_) => div()
                        .font_weight(FontWeight::SEMIBOLD)
                        .text_size(px(SMALL))
                        .child(member.name.chars().take(1).collect::<String>())
                        .into_any_element(),
                };
                div()
                    .flex()
                    .items_center()
                    .gap(px(8.0))
                    .py(px(8.0))
                    .border_b_1()
                    .border_color(c.line)
                    .child(
                        div()
                            .size(px(28.0))
                            .rounded(px(14.0))
                            .bg(c.surface)
                            .text_color(c.ink_2)
                            .flex()
                            .flex_none()
                            .items_center()
                            .justify_center()
                            .child(mark),
                    )
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .flex()
                            .flex_col()
                            .gap(px(2.0))
                            .child(
                                div()
                                    .truncate()
                                    .font_weight(FontWeight::SEMIBOLD)
                                    .child(member.name.to_owned()),
                            )
                            .child(member.details(cx)),
                    )
                    .children(action)
            })
            .collect();
        if rows.is_empty() {
            body = body.child(
                div()
                    .text_color(c.ink_2)
                    .child(room_waiting(cx, Some(room))),
            );
        }
        body = body.children(rows);
        let base = data.graph().read(cx).base();
        if !room.archived
            && !room.outside.is_empty()
            && let Some(base) = base
        {
            body = body.child(self.invite(
                format!("{}{}", base.trim_end_matches('/'), room.outside),
                cx,
            ));
        }
        scroller(&self.scroll, "chat-members", body)
    }

    fn invite(&self, address: String, cx: &Context<Self>) -> Div {
        let c = ui::colors(cx);
        let copy_label = if self.copied {
            words(cx, "복사됨", "Copied")
        } else {
            words(cx, "복사", "Copy")
        };
        div()
            .mt(px(12.0))
            .p(px(10.0))
            .flex()
            .flex_col()
            .gap(px(6.0))
            .rounded(px(8.0))
            .border_1()
            .border_color(c.line_strong)
            .bg(c.surface)
            .child(div().font_weight(FontWeight::SEMIBOLD).child(words(
                cx,
                "외부 에이전트 초대",
                "Invite an outside agent",
            )))
            .child(div().text_size(px(SMALL)).text_color(c.ink_2).child(words(
                cx,
                "주소를 받은 외부 에이전트는 이 방의 말을 읽고 씁니다.",
                "Anyone with this address can read and write in this room.",
            )))
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap(px(6.0))
                    .pl(px(8.0))
                    .p_1()
                    .rounded(px(6.0))
                    .bg(c.bg)
                    .border_1()
                    .border_color(c.line)
                    .child(
                        div()
                            .id("chat-outside-address")
                            .flex_1()
                            .min_w_0()
                            .truncate()
                            .text_size(px(SMALL))
                            .font_family(fonts::TERMINAL_FAMILY)
                            .tooltip(tip(address.clone()))
                            .child(address.clone()),
                    )
                    .child(
                        btn::button("chat-outside-copy", cx)
                            .role(Role::Button)
                            .aria_label(copy_label)
                            .flex_none()
                            .child(icon(
                                if self.copied {
                                    Lucide::Check
                                } else {
                                    Lucide::Copy
                                },
                                px(13.0),
                                c.ink_2,
                            ))
                            .child(copy_label)
                            .on_click(cx.listener(move |this, _, _, cx| {
                                cx.write_to_clipboard(ClipboardItem::new_string(address.clone()));
                                this.copied = true;
                                cx.notify();
                            })),
                    ),
            )
    }

    pub(super) fn terminal_panel(&mut self, cx: &mut Context<Self>) -> Div {
        let data = self.data.read(cx);
        let room = data.room();
        let members = room.map_or_else(Vec::new, |room| self.members(room, cx));
        let chosen = match &self.member {
            Some(key) => members.iter().find(|member| &member.key == key),
            None => members
                .iter()
                .find(|member| member.terminal().is_some())
                .or_else(|| members.first()),
        };
        let initial = chosen.map(|member| member.key.clone());
        let node = room
            .filter(|room| !room.archived)
            .and_then(|_| chosen.and_then(Member::terminal));
        let c = ui::colors(cx);
        let entity = cx.entity().downgrade();
        let rows: Vec<_> = if self.choosing {
            members
                .iter()
                .enumerate()
                .map(|(index, member)| {
                    let key = member.key.clone();
                    let active = chosen.is_some_and(|chosen| chosen.key == key);
                    let entity = entity.clone();
                    btn::button(("chat-terminal-member", index), cx)
                        .role(Role::Button)
                        .aria_label(member.name.to_owned())
                        .w_full()
                        .bg(if active { c.accent_wash } else { c.surface })
                        .child(div().min_w_0().truncate().child(member.name.to_owned()))
                        .children(member.terminal().is_none().then(|| {
                            div().ml_auto().text_color(c.ink_2).child(words(
                                cx,
                                "터미널 없음",
                                "No terminal",
                            ))
                        }))
                        .on_click(move |_, _, cx| {
                            let _ = entity.update(cx, |this, cx| {
                                this.member = Some(key.clone());
                                this.choosing = false;
                                this.scroll.reset();
                                cx.notify();
                            });
                        })
                })
                .collect()
        } else {
            Vec::new()
        };
        let head = !members.is_empty() || chosen.is_some();
        let head = head.then(|| {
            let title = chosen.map_or_else(
                || super::SideTab::Terminal.label(cx).to_owned(),
                |member| match *cx.global::<Lang>() {
                    Lang::Ko => format!("{}의 터미널", member.name),
                    Lang::En => format!("{}'s terminal", member.name),
                },
            );
            div()
                .flex_none()
                .px(px(12.0))
                .py(px(8.0))
                .flex()
                .items_center()
                .gap(px(8.0))
                .border_b_1()
                .border_color(c.line)
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .truncate()
                        .font_weight(FontWeight::SEMIBOLD)
                        .child(title),
                )
                .children(chosen.and_then(|member| member.harness).map(|harness| {
                    div()
                        .text_size(px(SMALL))
                        .text_color(c.ink_2)
                        .child(harness.to_owned())
                }))
                .children((members.len() > 1 || chosen.is_none()).then(|| {
                    let label = words(cx, "구성원 고르기", "Select member");
                    btn::button("chat-terminal-choose", cx)
                        .role(Role::Button)
                        .aria_label(label)
                        .aria_expanded(self.choosing)
                        .flex_none()
                        .child(label)
                        .child(icon(Lucide::ChevronDown, px(13.0), c.ink_2))
                        .on_click(cx.listener(|this, _, _, cx| {
                            this.choosing = !this.choosing;
                            cx.notify();
                        }))
                }))
                .children((members.len() == 1 && node.is_some()).then(|| {
                    div().text_color(c.ink_2).text_size(px(SMALL)).child(words(
                        cx,
                        "직접 칠 수 있습니다",
                        "Type in the terminal",
                    ))
                }))
        });
        if self.member.is_none() {
            self.member = initial;
        }
        self.terminal
            .update(cx, |terminal, cx| terminal.set_node(node, cx));
        let choices = (!rows.is_empty()).then(|| {
            scroller(
                &self.scroll,
                "chat-terminal-members",
                div()
                    .flex()
                    .flex_col()
                    .p(px(6.0))
                    .gap(px(4.0))
                    .children(rows),
            )
            .h(px(120.0))
            .flex_none()
            .border_b_1()
            .border_color(c.line)
        });
        div()
            .size_full()
            .flex()
            .flex_col()
            .children(head)
            .children(choices)
            .child(div().flex_1().min_h_0().child(self.terminal.clone()))
    }
}
