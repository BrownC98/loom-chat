mod agent;
mod link;
mod waits;

use serde::{Deserialize, Serialize};

pub use agent::{Model, StartDigests};
use api::graph::messages::{Kind, MessageKind};
pub use link::{
    AckRequest, AdapterRecord, ChoiceRequest, EndedTurn, Handoff, IdleReply, IdleRequest, ListenAt,
    ListenRequest, Report, Session, TakeRequest, Taken, TurnReply, WakeReply,
};
pub use waits::OpenQuestion;

pub const VERSION: &str = match option_env!("LOOM_VERSION") {
    Some(v) => v,
    None => env!("CARGO_PKG_VERSION"),
};

pub const COMMIT: Option<&str> = option_env!("LOOM_COMMIT");

#[derive(Clone, Copy, Debug)]
pub struct Versions {
    pub now: u32,
    pub oldest: u32,
}

impl Versions {
    pub fn accepts(self, version: u32) -> bool {
        (self.oldest..=self.now).contains(&version)
    }
}

pub const PTY_HOST: Versions = Versions { now: 5, oldest: 1 };
pub const NODE_CALLS: Versions = Versions { now: 2, oldest: 1 };
pub const NODE_CALLS_HEADER: &str = "loom-protocol";
pub const STREAMS: Versions = Versions { now: 2, oldest: 1 };
pub const DISTRO_HOST: u32 = 4;

#[derive(Clone, Copy, PartialEq, Eq, Debug, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum Deploy {
    #[default]
    Run,
    Skip,
}

impl Deploy {
    pub const SKIP_MARK: &str = "[skip ci]";
}

pub const LOOM: &str = "loom";
pub const HUMAN: &str = "human";

#[derive(Clone, Copy, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub struct MessageId(pub u32);

#[derive(Debug, PartialEq, Serialize, Deserialize)]
pub struct Delivery {
    pub id: MessageId,
    pub from: String,
    pub kind: Kind,
    pub body: String,
    #[serde(default)]
    pub what: Option<MessageKind>,
}

pub const BATCH_SEPARATOR: &str = "\n\n";
pub const END_TURN_NOW: &str = "End your turn now.";

impl Delivery {
    pub fn by_human(&self) -> bool {
        self.from == HUMAN
    }

    pub fn text(&self) -> String {
        let mode = match self.kind {
            Kind::Queue => "",
            Kind::Steer => ", mode: steer",
            Kind::Interrupt => ", mode: interrupt",
        };
        let note = MessageKind::Note;
        let what = self.what.as_ref().unwrap_or(&note);
        let from = if what.by_loom() { LOOM } else { &self.from };
        let mut lines = vec![format!(
            "loom message #{} (from: {from}, kind: {}{mode})",
            self.id.0,
            what.word()
        )];
        lines.extend(
            what.fields()
                .into_iter()
                .map(|(name, value)| format!("{}: {value}", name.word())),
        );
        if !self.body.is_empty() {
            if self.by_human() {
                lines.insert(0, self.body.clone());
            } else if !what.repeats_file(&self.body) {
                lines.push(self.body.clone());
            }
        }
        lines.join("\n")
    }
}

pub struct KindWord {
    pub word: &'static str,
    pub verb: &'static str,
    pub gate: bool,
}

const fn word(word: &'static str, verb: &'static str, gate: bool) -> KindWord {
    KindWord { word, verb, gate }
}

const KIND_WORDS: [KindWord; 15] = [
    word("USER DIRECTION", NO_HUMAN_WORD, false),
    word("USER DECISION", NO_HUMAN_WORD, false),
    word(
        "RULE UPDATE",
        "nothing for built-in skills: loom relaunches each node whose start text changed. For other rules, use `loom note` without this word",
        false,
    ),
    word("QUESTION", "`loom ask`", false),
    word("ESCALATION", "`loom ask`", false),
    word("ANSWER", "`loom answer <message-id>`", false),
    word(
        "STATUS",
        "`loom done` when your work is finished, else `loom note parent`",
        false,
    ),
    word("DONE", "`loom done`", false),
    word("READY", "`loom ready`", true),
    word("CI RESULT", "`loom verdict`", true),
    word("MERGED", "nothing: the merge queue sends kind merged", true),
    word("REWORK", "`loom verdict`", true),
    word("BLOCK", "`loom verdict`", true),
    word("VERDICT", "`loom verdict`", true),
    word("GATE", "`loom --help`", true),
];

const NO_HUMAN_WORD: &str =
    "nothing: loom sends the human's terminal input to your parent (kind human-input)";

pub fn kind_word(text: &str) -> Option<&'static KindWord> {
    let text = text.trim_start();
    KIND_WORDS.iter().find(|w| {
        text.strip_prefix(w.word)
            .is_some_and(|rest| !rest.starts_with(|c: char| c.is_alphanumeric() || c == '_'))
    })
}

pub fn gate_word(text: &str) -> Option<&'static str> {
    kind_word(text).filter(|w| w.gate).map(|w| w.word)
}

pub fn kind_line(text: &str) -> Option<&'static KindWord> {
    text.lines().find_map(kind_word)
}

pub const CHECK_SCRIPT: &str = "scripts/gate/check.sh";
pub const SHIP_SCRIPT: &str = "scripts/gate/ship.sh";
pub const SHIP_DEPLOY_FAILED: i32 = 11;

pub const REVIEW_RULES: &str = "# Review rules

Look first for the defects that the repository's AGENTS.md lists for review, in its order.

Read the owner's check record (result.md and logs/ in $LOOM_TASK_DIR: the light checks of the changed scope). Do not run the full checks and do not ask the owner for a full check log: the merge queue runs scripts/gate/check.sh once after the passes if the repository has it, and a failure goes back to the owner as rework. You can run one targeted test that shows a defect. The owner's brief, with its done-when, is brief.md in $LOOM_TASK_DIR.

Reviewers of one branch share their work. When you find a defect, send it at once to the other reviewer with `loom note <reviewer> --steer`. Do not check again a defect that the other reviewer sent or wrote. Answer a disagreement or a rejection with evidence: evidence decides, not authority. loom does not deliver a note to a reviewer that already judged this round: write evidence against its finding in your findings file.

On `kind: withdrawn`, write your findings to your findings file, then stop that review.";
