//! Standing permission rules: one human-editable TOML file, written
//! atomically. A rule decides what the guard does with an order on a point:
//! « always » lets it through, « never » refuses it. No rule means the
//! guard decides every time, as before.

use std::collections::BTreeMap;
use std::io;
use std::path::Path;

use moli_core::{PointId, Policy};

use crate::labels::write_atomic;

pub(crate) fn load(path: &Path) -> io::Result<BTreeMap<PointId, Policy>> {
    match std::fs::read_to_string(path) {
        Ok(text) => {
            toml::from_str(&text).map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))
        }
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(BTreeMap::new()),
        Err(e) => Err(e),
    }
}

pub(crate) async fn save(path: &Path, permissions: &BTreeMap<PointId, Policy>) -> io::Result<()> {
    let text = toml::to_string_pretty(permissions).map_err(io::Error::other)?;
    write_atomic(path, text.as_bytes()).await
}
