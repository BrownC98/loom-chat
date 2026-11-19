use std::fs::{File, Metadata, OpenOptions};
use std::io::{self, BufRead, BufReader, Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};
use std::sync::Arc;

use serde_json::Value;

mod blocks;

pub(crate) use blocks::message;

const PAGE_BYTES: u64 = 8 * 1024 * 1024;
const RECORD_BYTES: u64 = 16 * 1024 * 1024;
const PAGE_LINES: u64 = 4096;
const ANCHOR_BYTES: usize = 64;
const SUMMARY_BYTES: usize = 800;

#[derive(Debug, PartialEq, Eq)]
pub(crate) enum Kind {
    User(String),
    Assistant(String),
    ToolCall {
        id: Option<String>,
        name: String,
        summary: Option<String>,
    },
    ToolResult {
        id: Option<String>,
        summary: Option<String>,
        failed: Option<bool>,
    },
    Thinking(Option<String>),
    Status(Status),
}

#[derive(Debug, PartialEq, Eq)]
pub(crate) enum Status {
    TurnStarted,
    TurnEnded,
    TurnStopped,
    Notice(String),
}

#[derive(Debug, PartialEq, Eq)]
pub(crate) struct Item {
    pub line: u64,
    pub timestamp: Option<Arc<str>>,
    pub kind: Kind,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct ReadPosition {
    path: PathBuf,
    file: FileIdentity,
    byte: u64,
    line: u64,
    anchor: Vec<u8>,
    terminated: bool,
}

impl ReadPosition {
    pub fn byte(&self) -> u64 {
        self.byte
    }

    pub fn line(&self) -> u64 {
        self.line
    }
}

#[derive(Debug)]
pub(crate) struct Batch {
    pub items: Vec<Item>,
    pub cursor: ReadPosition,
    pub more: bool,
    pub reset: bool,
}

#[derive(Debug, PartialEq, Eq)]
pub(crate) enum Fallback {
    Unsupported,
    Missing,
    Unreadable,
    Format { line: u64 },
    RecordTooLarge { line: u64 },
}

impl Fallback {
    pub fn reason(&self) -> &'static str {
        match self {
            Self::Unsupported => "이 실행 환경의 세션 기록은 지원하지 않습니다.",
            Self::Missing => "세션 기록 파일이 아직 없습니다.",
            Self::Unreadable => "세션 기록 파일을 열거나 읽지 못했습니다.",
            Self::Format { .. } => "이 CLI 판의 세션 기록을 읽지 못했습니다.",
            Self::RecordTooLarge { .. } => "세션 기록의 한 줄이 읽기 한도를 넘었습니다.",
        }
    }
}

impl From<io::Error> for Fallback {
    fn from(error: io::Error) -> Self {
        match error.kind() {
            io::ErrorKind::NotFound => Self::Missing,
            _ => Self::Unreadable,
        }
    }
}

pub(crate) struct Format;

pub(crate) type Parsed = Result<(), Format>;
pub(crate) type Decode = fn(&Value, &mut Vec<Kind>) -> Parsed;

pub(crate) struct Source {
    path: PathBuf,
    decode: Decode,
}

impl Source {
    pub fn jsonl(path: PathBuf, decode: Decode) -> Result<Self, Fallback> {
        if !path.is_absolute() {
            return Err(Fallback::Unreadable);
        }
        if !path.extension().is_some_and(|ext| ext == "jsonl") {
            return Err(Fallback::Format { line: 1 });
        }
        Ok(Self { path, decode })
    }

    pub fn read(&self, after: Option<&ReadPosition>) -> Result<Batch, Fallback> {
        let (mut file, metadata) = open(&self.path)?;
        let identity = file_identity(&file, &metadata)?;
        let len = metadata.len();
        let previous = match after {
            Some(cursor)
                if cursor.path == self.path
                    && cursor.file == identity
                    && cursor.byte <= len
                    && cursor.anchor == anchor(&mut file, cursor.byte)? =>
            {
                Some(cursor)
            }
            _ => None,
        };
        let mut byte = previous.map_or(0, ReadPosition::byte);
        let mut line = previous.map_or(0, ReadPosition::line);
        let mut terminated = previous.is_none_or(|cursor| cursor.terminated);
        file.seek(SeekFrom::Start(byte))?;
        let mut reader = BufReader::new(file.take(len - byte));
        let start = byte;
        let first_line = line;
        let mut tail = previous.map_or_else(Vec::new, |cursor| cursor.anchor.clone());
        let mut items = Vec::new();
        let mut kinds = Vec::new();
        let mut raw = Vec::new();
        while byte < len && byte - start < PAGE_BYTES && line - first_line < PAGE_LINES {
            raw.clear();
            let size = Read::by_ref(&mut reader)
                .take(RECORD_BYTES + 1)
                .read_until(b'\n', &mut raw)? as u64;
            if size == 0 {
                break;
            }
            if size > RECORD_BYTES {
                return Err(Fallback::RecordTooLarge { line: line + 1 });
            }
            let newline = raw.ends_with(b"\n");
            if !terminated {
                match raw.as_slice() {
                    b"\n" | b"\r\n" => {
                        extend_anchor(&mut tail, &raw);
                        byte += size;
                        terminated = true;
                        continue;
                    }
                    b"\r" => break,
                    _ => return Err(Fallback::Format { line: line + 1 }),
                }
            }
            if !raw.iter().all(u8::is_ascii_whitespace) {
                let entry: Value = match serde_json::from_slice(&raw) {
                    Ok(entry) => entry,
                    Err(error) if !newline && error.is_eof() => break,
                    Err(_) => return Err(Fallback::Format { line: line + 1 }),
                };
                (self.decode)(&entry, &mut kinds)
                    .map_err(|_| Fallback::Format { line: line + 1 })?;
                let timestamp = entry["timestamp"].as_str().map(Arc::<str>::from);
                items.extend(kinds.drain(..).map(|kind| Item {
                    line: line + 1,
                    timestamp: timestamp.clone(),
                    kind,
                }));
            } else if !newline {
                break;
            }
            extend_anchor(&mut tail, &raw);
            byte += size;
            line += 1;
            terminated = newline;
        }
        let more = byte < len && (byte - start >= PAGE_BYTES || line - first_line >= PAGE_LINES);
        Ok(Batch {
            items,
            cursor: ReadPosition {
                path: self.path.clone(),
                file: identity,
                byte,
                line,
                anchor: tail,
                terminated,
            },
            more,
            reset: after.is_some() && previous.is_none(),
        })
    }
}

fn open(path: &Path) -> Result<(File, Metadata), Fallback> {
    let mut options = OpenOptions::new();
    options.read(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.custom_flags(libc::O_NONBLOCK);
    }
    let file = options.open(path)?;
    let metadata = file.metadata()?;
    if !metadata.is_file() {
        return Err(Fallback::Unreadable);
    }
    Ok((file, metadata))
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct FileIdentity {
    volume: u64,
    index: u64,
}

#[cfg(unix)]
fn file_identity(_: &File, metadata: &Metadata) -> io::Result<FileIdentity> {
    use std::os::unix::fs::MetadataExt;

    Ok(FileIdentity {
        volume: metadata.dev(),
        index: metadata.ino(),
    })
}

#[cfg(windows)]
fn file_identity(file: &File, _: &Metadata) -> io::Result<FileIdentity> {
    use std::os::windows::io::AsRawHandle;
    use windows_sys::Win32::Storage::FileSystem::{
        BY_HANDLE_FILE_INFORMATION, GetFileInformationByHandle,
    };

    let mut info = BY_HANDLE_FILE_INFORMATION::default();
    if unsafe { GetFileInformationByHandle(file.as_raw_handle(), &mut info) } == 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(FileIdentity {
        volume: info.dwVolumeSerialNumber.into(),
        index: (u64::from(info.nFileIndexHigh) << 32) | u64::from(info.nFileIndexLow),
    })
}

fn extend_anchor(anchor: &mut Vec<u8>, bytes: &[u8]) {
    let bytes = &bytes[bytes.len().saturating_sub(ANCHOR_BYTES)..];
    let drop = (anchor.len() + bytes.len()).saturating_sub(ANCHOR_BYTES);
    anchor.drain(..drop);
    anchor.extend_from_slice(bytes);
}

fn anchor(file: &mut File, byte: u64) -> io::Result<Vec<u8>> {
    let start = byte.saturating_sub(ANCHOR_BYTES as u64);
    let mut bytes = vec![0; (byte - start) as usize];
    file.seek(SeekFrom::Start(start))?;
    file.read_exact(&mut bytes)?;
    Ok(bytes)
}

pub(crate) fn summary(text: &str) -> Option<String> {
    let text = text.trim();
    if text.is_empty() {
        return None;
    }
    if text.len() <= SUMMARY_BYTES {
        return Some(text.to_owned());
    }
    Some(format!(
        "{}…",
        &text[..text.floor_char_boundary(SUMMARY_BYTES)]
    ))
}

pub(crate) fn input_summary(input: &Value) -> Option<String> {
    [
        "command",
        "file_path",
        "path",
        "query",
        "pattern",
        "description",
    ]
    .into_iter()
    .find_map(|key| input[key].as_str().and_then(summary))
    .or_else(|| input.as_str().and_then(summary))
}

pub(crate) fn output_summary(output: &Value) -> Option<String> {
    output.as_str().and_then(summary).or_else(|| {
        output.as_array()?.iter().find_map(|part| {
            part.as_str()
                .or_else(|| part["text"].as_str())
                .and_then(summary)
        })
    })
}

pub(crate) fn identifier(value: &Value) -> Option<String> {
    value
        .as_str()
        .filter(|id| !id.is_empty())
        .map(str::to_owned)
}
