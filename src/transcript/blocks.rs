use serde_json::Value;

use super::{Format, Kind, Parsed, identifier, input_summary, output_summary, summary};

pub(crate) fn message(role: &str, content: &Value, items: &mut Vec<Kind>) -> Parsed {
    let text = match role {
        "user" => Kind::User,
        "assistant" => Kind::Assistant,
        _ => return Err(Format),
    };
    if let Some(content) = content.as_str() {
        if !content.is_empty() {
            items.push(text(content.to_owned()));
        }
        return Ok(());
    }

    let mut pending = String::new();
    for part in content.as_array().ok_or(Format)? {
        let part_type = part["type"].as_str().ok_or(Format)?;
        if part_type == "text" {
            pending.push_str(part["text"].as_str().ok_or(Format)?);
            continue;
        }
        if !pending.is_empty() {
            items.push(text(std::mem::take(&mut pending)));
        }
        let item = match part_type {
            "tool_use" | "server_tool_use" => Kind::ToolCall {
                id: identifier(&part["id"]),
                name: part["name"].as_str().ok_or(Format)?.to_owned(),
                summary: input_summary(&part["input"]),
            },
            "tool_result" => Kind::ToolResult {
                id: identifier(&part["tool_use_id"]),
                summary: output_summary(&part["content"]),
                failed: part["is_error"].as_bool(),
            },
            "thinking" => Kind::Thinking(part["thinking"].as_str().and_then(summary)),
            "redacted_thinking" => Kind::Thinking(None),
            "image" | "document" | "tool_reference" | "tool_addition" | "tool_removal" => continue,
            _ => return Err(Format),
        };
        items.push(item);
    }
    if !pending.is_empty() {
        items.push(text(pending));
    }
    Ok(())
}
