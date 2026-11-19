use api::graph::waits::{PromptKey, WaitKind};

use crate::strings::Str;

pub fn kind_label(kind: WaitKind) -> Str {
    match kind {
        WaitKind::Question => Str::WaitQuestion,
        WaitKind::Approval => Str::WaitApproval,
        WaitKind::Trust => Str::WaitTrust,
        WaitKind::Auth => Str::WaitAuth,
        WaitKind::Input => Str::WaitInput,
        WaitKind::Done => Str::WaitDone,
        WaitKind::Reply => Str::WaitReply,
        WaitKind::Stopped => Str::WaitStopped,
        WaitKind::Limit => Str::WaitLimit,
    }
}

pub fn key_label(key: &PromptKey, index: usize) -> String {
    match key {
        PromptKey::Char(c) => c.to_string(),
        PromptKey::Line(text) => text.chars().next().map(String::from).unwrap_or_default(),
        PromptKey::Tab => "tab".to_owned(),
        PromptKey::BackTab => "\u{21e7}tab".to_owned(),
        PromptKey::Move(_) | PromptKey::Tick(_) | PromptKey::Reply(_) => (index + 1).to_string(),
        PromptKey::Enter => "\u{23ce}".to_owned(),
    }
}
