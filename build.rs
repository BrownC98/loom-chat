use std::collections::BTreeMap;
use std::io::{self, Write};
use std::path::{Path, PathBuf};

use flate2::{Compression, write::GzEncoder};
use schemars::schema::{RootSchema, Schema};
use serde_json::Value;
use typify::{TypeSpace, TypeSpaceSettings};

#[path = "src/distro/linux_binary.rs"]
mod linux_binary;

const SCHEMA_DIR: &str = "src/profile/codex/schema";
const EMBED_LINUX: &str = "LOOM_EMBED_LINUX";

fn main() {
    println!("cargo::rerun-if-changed={SCHEMA_DIR}");
    println!("cargo::rerun-if-changed=src/db/changes");
    println!("cargo::rerun-if-env-changed={EMBED_LINUX}");
    println!("cargo::rustc-check-cfg=cfg(loom_embed_linux)");
    let out = PathBuf::from(std::env::var_os("OUT_DIR").expect("cargo가 OUT_DIR을 준다"));
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("windows")
        && let Some(source) = std::env::var_os(EMBED_LINUX)
    {
        embed_linux(&out, Path::new(&source)).expect("WSL용 실행 파일을 내장한다");
    }
    let mut definitions: BTreeMap<String, Value> = BTreeMap::new();
    let mut files: Vec<_> = std::fs::read_dir(SCHEMA_DIR)
        .expect("스키마 폴더가 있다")
        .map(|e| e.expect("스키마 폴더를 읽는다").path())
        .collect();
    files.sort();
    for path in files {
        let mut root: Value =
            serde_json::from_slice(&std::fs::read(&path).expect("스키마 파일을 읽는다"))
                .expect("스키마 파일은 JSON이다");
        let root = root.as_object_mut().expect("스키마 최상위는 객체다");
        let defs = root.remove("definitions").unwrap_or_default();
        root.remove("$schema");
        let name = path
            .file_stem()
            .and_then(|s| s.to_str())
            .expect("파일 이름");
        let defs = defs.as_object().cloned().unwrap_or_default();
        for (key, def) in defs
            .into_iter()
            .chain([(name.to_owned(), Value::Object(root.clone()))])
        {
            match definitions.get(&key) {
                Some(seen) if *seen != def => panic!("{key}: 스키마 파일마다 정의가 다르다"),
                _ => {
                    definitions.insert(key, def);
                }
            }
        }
    }
    let definitions = definitions
        .into_iter()
        .map(|(k, v)| {
            (
                k,
                serde_json::from_value::<Schema>(v).expect("JSON Schema다"),
            )
        })
        .collect();
    let mut space = TypeSpace::new(&TypeSpaceSettings::default());
    space
        .add_root_schema(RootSchema {
            definitions,
            ..Default::default()
        })
        .expect("typify가 스키마를 받는다");
    std::fs::write(out.join("codex_protocol.rs"), space.to_stream().to_string())
        .expect("생성한 타입을 쓴다");
}

fn embed_linux(out: &Path, source: &Path) -> io::Result<()> {
    println!("cargo::rerun-if-changed={}", source.display());
    let bytes = std::fs::read(source)?;
    let name = linux_binary::hashed_name(bytes.as_slice())?;
    let mut gzip = GzEncoder::new(
        std::fs::File::create(out.join("loom-linux.gz"))?,
        Compression::default(),
    );
    gzip.write_all(&bytes)?;
    gzip.finish()?;
    println!("cargo::rustc-cfg=loom_embed_linux");
    println!("cargo::rustc-env=LOOM_EMBED_LINUX_NAME={name}");
    println!("cargo::rustc-env=LOOM_EMBED_LINUX_LEN={}", bytes.len());
    Ok(())
}
