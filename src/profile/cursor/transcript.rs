use serde_json::Value;

use crate::transcript::{Format, Kind, Parsed, Status, message};

pub(super) fn decode(entry: &Value, items: &mut Vec<Kind>) -> Parsed {
    match entry["role"].as_str() {
        Some(role @ ("user" | "assistant")) => {
            message(role, &entry["message"]["content"], items)?;
        }
        None if entry["type"] == "turn_ended" => {
            let status = match entry["status"].as_str().ok_or(Format)? {
                "success" | "completed" => Status::TurnEnded,
                "aborted" | "error" => Status::TurnStopped,
                _ => return Err(Format),
            };
            items.push(Kind::Status(status));
        }
        _ => return Err(Format),
    }
    Ok(())
}
