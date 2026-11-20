use std::io::{BufRead, Read};
use std::process::{Command, Stdio};
use std::sync::mpsc;
use std::time::{Duration, Instant};

use crate::strings::Lang;

const READY_PREFIX: &str = "loom: ";
const START_LIMIT: Duration = Duration::from_secs(30);
const VERSION_LIMIT: Duration = Duration::from_secs(5);
const REAP_POLL: Duration = Duration::from_millis(200);

fn command(arg: &str) -> std::io::Result<Command> {
    let mut command = Command::new(std::env::current_exe()?);
    command
        .arg(arg)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null());
    Ok(command)
}

pub fn version() -> Option<String> {
    let mut command = command("--version").ok()?;
    let mut child = command.spawn().ok()?;
    let mut stdout = child.stdout.take()?;
    let (sent, received) = mpsc::channel();
    std::thread::spawn(move || {
        let mut text = String::new();
        let _ = stdout.read_to_string(&mut text);
        let _ = sent.send(text);
    });
    let text = received.recv_timeout(VERSION_LIMIT).ok();
    let _ = child.kill();
    let _ = child.wait();
    let text = text?;
    let version = text.trim().strip_prefix("loom ")?;
    (!version.is_empty() && !version.contains(char::is_whitespace)).then(|| version.to_owned())
}

pub enum StartError {
    Launch,
    NoReply,
    NotReady,
}

impl StartError {
    pub fn text(self, lang: Lang) -> &'static str {
        match (self, lang) {
            (Self::Launch, Lang::Ko) => "데몬을 실행할 수 없습니다. 앱을 다시 시작해 주세요.",
            (Self::Launch, Lang::En) => "Could not launch the daemon. Restart the app.",
            (Self::NoReply, Lang::Ko) => "데몬이 제시간에 응답하지 않았습니다. 다시 시도해 주세요.",
            (Self::NoReply, Lang::En) => "The daemon did not respond in time. Try again.",
            (Self::NotReady, Lang::Ko) => {
                "데몬을 시작하지 못했습니다. 데이터 폴더의 serve.log를 확인해 주세요."
            }
            (Self::NotReady, Lang::En) => {
                "Could not start the daemon. Check serve.log in the data folder."
            }
        }
    }
}

pub fn start() -> Result<String, StartError> {
    let mut command = command("start").map_err(|_| StartError::Launch)?;
    let mut child = command.spawn().map_err(|_| StartError::Launch)?;
    let Some(stdout) = child.stdout.take() else {
        let _ = child.kill();
        let _ = child.wait();
        return Err(StartError::NotReady);
    };
    let deadline = Instant::now() + START_LIMIT;
    let (sent, received) = mpsc::channel();
    std::thread::spawn(move || {
        let mut line = String::new();
        let read = std::io::BufReader::new(stdout).read_line(&mut line);
        let _ = sent.send(read.map(|_| line));
    });
    let line = match received.recv_timeout(START_LIMIT) {
        Ok(read) => read.map_err(|_| StartError::NotReady),
        Err(_) => Err(StartError::NoReply),
    };
    let base = line.and_then(|line| {
        line.trim()
            .strip_prefix(READY_PREFIX)
            .filter(|base| !base.is_empty())
            .map(str::to_owned)
            .ok_or(StartError::NotReady)
    });
    if base.is_err() {
        let _ = child.kill();
        let _ = child.wait();
        return base;
    }
    std::thread::spawn(move || {
        while Instant::now() < deadline {
            if !matches!(child.try_wait(), Ok(None)) {
                return;
            }
            std::thread::sleep(REAP_POLL);
        }
        let _ = child.kill();
        let _ = child.wait();
    });
    base
}
