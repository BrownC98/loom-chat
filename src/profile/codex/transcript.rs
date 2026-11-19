use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use serde_json::Value;

use crate::transcript::{
    Fallback, Format, Kind, Parsed, Status, identifier, input_summary, output_summary, summary,
};

pub(super) fn rollout(home: &Path, thread: &str) -> Result<PathBuf, Fallback> {
    if thread.len() != 36
        || !thread.bytes().enumerate().all(|(at, byte)| match at {
            8 | 13 | 18 | 23 => byte == b'-',
            _ => byte.is_ascii_hexdigit(),
        })
    {
        return Err(Fallback::Unreadable);
    }
    let suffix = format!("-{thread}.jsonl");
    if let Some(path) = find(&home.join("sessions"), super::DATE_DIRS, &suffix)? {
        return Ok(path);
    }
    find(&home.join("archived_sessions"), 0, &suffix)?.ok_or(Fallback::Missing)
}

fn find(dir: &Path, depth: usize, suffix: &str) -> io::Result<Option<PathBuf>> {
    let entries = match fs::read_dir(dir) {
        Ok(entries) => entries,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error),
    };
    for entry in entries {
        let entry = entry?;
        let name = entry.file_name();
        let Some(name) = name.to_str() else {
            continue;
        };
        if depth > 0 && name.bytes().all(|byte| byte.is_ascii_digit()) {
            if entry.file_type()?.is_dir()
                && let Some(path) = find(&entry.path(), depth - 1, suffix)?
            {
                return Ok(Some(path));
            }
        } else if depth == 0 && name.starts_with("rollout-") && name.ends_with(suffix) {
            return Ok(Some(entry.path()));
        }
    }
    Ok(None)
}

pub(super) fn decode(entry: &Value, items: &mut Vec<Kind>) -> Parsed {
    let payload = &entry["payload"];
    match entry["type"].as_str().ok_or(Format)? {
        "event_msg" => event(payload, items)?,
        "response_item" => items.extend(response(payload)?),
        "session_meta" | "turn_context" | "compacted" | "token_usage_record" | "world_state" => {
            if !payload.is_object() {
                return Err(Format);
            }
        }
        _ => return Err(Format),
    }
    Ok(())
}

fn event(payload: &Value, items: &mut Vec<Kind>) -> Parsed {
    let kind = match payload["type"].as_str().ok_or(Format)? {
        "item_completed" => return completed(&payload["item"], items),
        "user_message" => message(
            payload["message"].as_str().ok_or(Format)?.to_owned(),
            Kind::User,
        ),
        "agent_message" => message(
            payload["message"].as_str().ok_or(Format)?.to_owned(),
            Kind::Assistant,
        ),
        "agent_reasoning" => Kind::Thinking(payload["text"].as_str().and_then(summary)),
        "task_started" | "turn_started" => Kind::Status(Status::TurnStarted),
        "task_complete" | "turn_completed" => Kind::Status(Status::TurnEnded),
        "turn_aborted" => Kind::Status(Status::TurnStopped),
        "error" => Kind::Status(Status::Notice(
            payload["message"]
                .as_str()
                .and_then(summary)
                .ok_or(Format)?,
        )),
        "item_started"
        | "token_count"
        | "thread_settings_applied"
        | "agent_reasoning_raw_content"
        | "context_compacted"
        | "exec_command_begin"
        | "exec_command_end"
        | "patch_apply_begin"
        | "patch_apply_end"
        | "mcp_tool_call_begin"
        | "mcp_tool_call_end"
        | "web_search_begin"
        | "web_search_end"
        | "warning" => return Ok(()),
        _ => return Err(Format),
    };
    items.push(kind);
    Ok(())
}

fn completed(item: &Value, items: &mut Vec<Kind>) -> Parsed {
    let kind = match item["type"].as_str().ok_or(Format)? {
        "UserMessage" | "userMessage" => message(text(&item["content"])?, Kind::User),
        "AgentMessage" | "agentMessage" => message(text(&item["content"])?, Kind::Assistant),
        "Reasoning" | "reasoning" => Kind::Thinking(output_summary(&item["summary_text"])),
        "Extension" | "extension" if item["kind"] == "web.search" => {
            let id = identifier(&item["id"]);
            items.push(Kind::ToolCall {
                id: id.clone(),
                name: "web.search".to_owned(),
                summary: item["query"].as_str().and_then(summary),
            });
            Kind::ToolResult {
                id,
                summary: item["results"]
                    .as_array()
                    .and_then(|results| results.first())
                    .and_then(|result| result["snippet"].as_str())
                    .and_then(summary),
                failed: None,
            }
        }
        "CommandExecution"
        | "commandExecution"
        | "ImageView"
        | "imageView"
        | "FileChange"
        | "fileChange"
        | "McpToolCall"
        | "mcpToolCall"
        | "DynamicToolCall"
        | "dynamicToolCall"
        | "WebSearch"
        | "webSearch"
        | "Plan"
        | "plan"
        | "ContextCompaction"
        | "contextCompaction"
        | "CollabAgentToolCall"
        | "collabAgentToolCall"
        | "EnteredReviewMode"
        | "ExitedReviewMode"
        | "Extension"
        | "extension" => return Ok(()),
        _ => return Err(Format),
    };
    items.push(kind);
    Ok(())
}

fn message(text: String, kind: fn(String) -> Kind) -> Kind {
    if text.trim().is_empty() {
        Kind::Thinking(None)
    } else {
        kind(text)
    }
}

fn text(content: &Value) -> Result<String, Format> {
    if let Some(text) = content.as_str() {
        return Ok(text.to_owned());
    }
    let mut text = String::new();
    for part in content.as_array().ok_or(Format)? {
        match part["type"].as_str().ok_or(Format)? {
            "Text" | "text" => text.push_str(part["text"].as_str().ok_or(Format)?),
            "Image" | "image" | "LocalImage" | "localImage" | "Skill" | "skill" | "Mention"
            | "mention" => {}
            _ => return Err(Format),
        }
    }
    Ok(text)
}

fn response(payload: &Value) -> Result<Option<Kind>, Format> {
    Ok(Some(match payload["type"].as_str().ok_or(Format)? {
        "function_call" | "custom_tool_call" | "local_shell_call" => {
            let name = if payload["type"] == "local_shell_call" {
                "Shell"
            } else {
                payload["name"].as_str().ok_or(Format)?
            };
            let arguments = payload["arguments"]
                .as_str()
                .and_then(|arguments| serde_json::from_str::<Value>(arguments).ok());
            Kind::ToolCall {
                id: identifier(&payload["call_id"]),
                name: name.to_owned(),
                summary: input_summary(arguments.as_ref().unwrap_or(&payload["input"]))
                    .or_else(|| input_summary(&payload["arguments"])),
            }
        }
        "function_call_output" | "custom_tool_call_output" => {
            let output = &payload["output"];
            Kind::ToolResult {
                id: identifier(&payload["call_id"]),
                summary: output_summary(output.get("content").unwrap_or(output)),
                failed: output["success"].as_bool().map(|success| !success),
            }
        }
        "message"
        | "reasoning"
        | "additional_tools"
        | "compaction"
        | "web_search_call"
        | "image_generation_call"
        | "ghost_snapshot" => return Ok(None),
        _ => return Err(Format),
    }))
}
