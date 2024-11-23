use anyhow::anyhow;
use std::{fmt, path::PathBuf};

// Motivation for this struct
// - Currently `clap` doesn't support parsing into `Box<[char; 40]>`
// - Box<[char; 40]> is unergonomic because there is no AsRef<Path> for Box<[char;N]>.
// (Because that's not how Path are represented internally)
#[derive(Clone, Debug)]
pub(super) struct GitHexHash {
    // A SHA-1 has in it's hexadecimal representation.
    // We could also represent this as Box<[u8; 20]> with the real hash value
    // but as we mostly uses this as a Path this more convenient to do it this way.
    hex: Box<[u8; 40]>,
}

impl GitHexHash {
    pub(super) fn to_path(&self) -> PathBuf {
        let (dir, rest) = self.as_str().split_at(2);
        PathBuf::from(".git/objects").join(dir).join(rest)
    }

    pub(super) fn as_str(&self) -> &str {
        // TODO: maybe just representing this as a `str` would be more convenient?
        // SAFETY: We know self.hex only contains valid ASCII characters
        unsafe { std::str::from_utf8_unchecked(self.hex.as_slice()) }
    }
}

impl TryFrom<&str> for GitHexHash {
    type Error = anyhow::Error;

    fn try_from(value: &str) -> Result<Self, Self::Error> {
        let hash = value
            .as_bytes()
            .iter()
            .map(|&c| {
                if c.is_ascii_hexdigit() {
                    Ok(c)
                } else {
                    Err(anyhow!("'{c}' is not ASCII hex digit"))
                }
            })
            .collect::<Result<Vec<_>, _>>()?;
        let hex = hash
            .into_boxed_slice()
            .try_into()
            .map_err(|bad: Box<[u8]>| anyhow!("wrong hash length: {}", bad.len()))?;
        Ok(Self { hex })
    }
}

impl fmt::Display for GitHexHash {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.as_str())
    }
}
