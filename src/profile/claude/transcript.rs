use serde_json::Value;

use crate::transcript::{Format, Kind, Parsed, Status, message, summary};

pub(super) fn decode(entry: &Value, items: &mut Vec<Kind>) -> Parsed {
    match entry["type"].as_str().ok_or(Format)? {
        role @ ("user" | "assistant") => {
            if entry["isSidechain"] == true
                || entry["isMeta"] == true
                || entry["isVisibleInTranscriptOnly"] == true
                || entry["message"]["model"] == super::SYNTHETIC_MODEL
            {
                return Ok(());
            }
            let body = &entry["message"];
            if body["role"] != role {
                return Err(Format);
            }
            message(role, &body["content"], items)?;
        }
        "system" => {
            if entry["isSidechain"] == true {
                return Ok(());
            }
            let status = match entry["subtype"].as_str().ok_or(Format)? {
                "turn_duration" => Some(Status::TurnEnded),
                "api_error" | "informational" | "away_summary" => entry["content"]
                    .as_str()
                    .and_then(summary)
                    .map(Status::Notice),
                _ => None,
            };
            items.extend(status.map(Kind::Status));
        }
        "agent-name"
        | "ai-title"
        | "api-request"
        | "api-request-blob"
        | "api-request-shape"
        | "atis-latch"
        | "attachment"
        | "cost-state"
        | "custom-title"
        | "file-history-delta"
        | "file-history-snapshot"
        | "last-prompt"
        | "mode"
        | "permission-mode"
        | "progress"
        | "queue-operation"
        | "session-owner"
        | "summary"
        | "tag"
        | "tools-used" => {}
        _ => return Err(Format),
    }
    Ok(())
}
