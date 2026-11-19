use std::fmt::Write;

use api::ids::NodeId;
use gpui::{App, Entity, Task};

use crate::link;
use crate::problem::Failure;
use crate::store::Store;
use crate::strings::Lang;
use crate::views::pictures::Attachment;

pub(super) struct Delivery {
    pub files: Vec<Attachment>,
    pub recipients: Vec<(NodeId, String)>,
}

impl Delivery {
    pub fn upload(self, graph: &Entity<Store>, cx: &App) -> Task<Result<String, String>> {
        if self.files.is_empty() {
            return Task::ready(Ok(String::new()));
        }
        let lang = *cx.global::<Lang>();
        graph.read(cx).call(cx, move |base| {
            let mut text = "\n\n첨부 파일".to_owned();
            let show_recipient = self.recipients.len() > 1;
            for (node, name) in self.recipients {
                if show_recipient {
                    let _ = write!(text, "\n{name} · {node}");
                }
                for file in &self.files {
                    let (mime, bytes) = file.payload();
                    let path = link::save_paste(base, Some(node), &file.name, mime, bytes)
                        .map_err(|error| {
                            Failure::Other(format!("{name} · {} · {}", file.name, error.text(lang)))
                        })?;
                    text.push('\n');
                    text.push_str(&path);
                }
            }
            Ok(text)
        })
    }
}
