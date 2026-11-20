use serde_json::Value;

use crate::strings::{Lang, Str};
use api::graph::errors::{LaunchError, Problem};

type Vars = Vec<(String, String)>;

pub enum Failure {
    Refused { status: u16, body: String },
    Other(String),
    Transport(Transport),
}

#[derive(Clone, Copy)]
pub enum Transport {
    Connection,
    Timeout,
    Request,
    Response,
}

impl Transport {
    pub fn text(self, lang: Lang) -> &'static str {
        match (self, lang) {
            (Self::Connection, Lang::Ko) => "데몬에 연결할 수 없습니다. 다시 연결해 주세요.",
            (Self::Connection, Lang::En) => "Could not reach the daemon. Reconnect and try again.",
            (Self::Timeout, Lang::Ko) => "데몬이 제시간에 응답하지 않았습니다.",
            (Self::Timeout, Lang::En) => "The daemon did not respond in time.",
            (Self::Request, Lang::Ko) => "요청을 데몬에 보낼 수 없습니다.",
            (Self::Request, Lang::En) => "Could not send the request to the daemon.",
            (Self::Response, Lang::Ko) => "데몬의 응답을 읽을 수 없습니다.",
            (Self::Response, Lang::En) => "Could not read the daemon response.",
        }
    }
}

impl Failure {
    pub fn text(&self, lang: Lang) -> String {
        match self {
            Failure::Refused { status, body } => text(body, lang).unwrap_or_else(|| {
                Str::ProblemStatus.with(lang, &[("status", &status.to_string())])
            }),
            Failure::Other(text) => text.clone(),
            Failure::Transport(error) => error.text(lang).to_owned(),
        }
    }
    pub fn refusal(&self) -> Option<String> {
        let Failure::Refused { body, .. } = self else {
            return None;
        };
        let problem: Value = serde_json::from_str(body).ok()?;
        (problem.get("code")?.as_str()? == "refused")
            .then(|| problem.get("value").and_then(code).map(str::to_owned))
            .flatten()
    }

    pub fn launch_error(&self) -> Option<LaunchError> {
        let Failure::Refused { body, .. } = self else {
            return None;
        };
        match serde_json::from_str(body).ok()? {
            Problem::Launch(launch) => Some(launch),
            _ => None,
        }
    }
}

fn text(body: &str, lang: Lang) -> Option<String> {
    let problem: Value = serde_json::from_str(body).ok()?;
    let value = problem.get("value");
    let (keys, vars): (Vec<String>, Vars) = match problem.get("code")?.as_str()? {
        "refused" => (
            vec![format!("problem.refused.{}", code(value?)?)],
            vars(value?),
        ),
        "repo" => (
            vec![format!("problem.repo.{}", code(value?)?)],
            vars(value?),
        ),
        "launch" => {
            let launch = value?;
            if code(launch)? == "wsl" {
                let inner = launch.get("value")?;
                (vec![format!("problem.wsl.{}", code(inner)?)], fields(inner))
            } else {
                let key = format!("problem.launch.{}", code(launch)?);
                let vars = vars(launch);
                let wsl = vars.iter().any(|(name, _)| name == "distro");
                let keys = if wsl {
                    vec![format!("{key}_wsl"), key]
                } else {
                    vec![key]
                };
                (keys, vars)
            }
        }
        "noLogin" => (vec!["problem.noLogin".to_owned()], Vars::new()),
        "internal" => (
            vec!["problem.internal".to_owned()],
            vec![("value".to_owned(), joined(value?))],
        ),
        _ => return None,
    };
    let template = keys.iter().find_map(|key| Str::find(key))?;
    let args: Vec<(&str, &str)> = vars.iter().map(|(k, v)| (k.as_str(), v.as_str())).collect();
    Some(template.with(lang, &args))
}

fn code(coded: &Value) -> Option<&str> {
    coded.get("code")?.as_str()
}

fn vars(coded: &Value) -> Vars {
    match coded.get("value") {
        Some(value @ Value::Object(_)) => fields(value),
        Some(value) => vec![("value".to_owned(), joined(value))],
        None => Vars::new(),
    }
}

fn fields(value: &Value) -> Vars {
    value
        .as_object()
        .map(|map| map.iter().map(|(k, v)| (k.clone(), joined(v))).collect())
        .unwrap_or_default()
}

fn joined(value: &Value) -> String {
    match value {
        Value::Array(items) => items.iter().map(scalar).collect::<Vec<_>>().join(", "),
        other => scalar(other),
    }
}

fn scalar(value: &Value) -> String {
    match value {
        Value::String(text) => text.clone(),
        Value::Null => String::new(),
        other => other.to_string(),
    }
}
