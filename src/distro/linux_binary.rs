use std::io::{self, Read};

use sha2::{Digest, Sha256};

pub const LINUX_LOOM: &str = "loom-linux";
const LINUX_HASH_BYTES: usize = 6;

pub fn hashed_name(mut source: impl Read) -> io::Result<String> {
    let mut hash = Sha256::new();
    let mut buf = vec![0; 1 << 16];
    loop {
        match source.read(&mut buf)? {
            0 => break,
            n => hash.update(&buf[..n]),
        }
    }
    let hex: String = hash.finalize()[..LINUX_HASH_BYTES]
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect();
    Ok(format!("{LINUX_LOOM}-{hex}"))
}
