use anyhow::Result;
use std::path::{Path, PathBuf};

pub(crate) mod content;
pub(crate) mod doctor;
pub(crate) mod output;
pub(crate) mod project;
pub(crate) mod scaffold;

pub(crate) fn absolute_path(path: &Path) -> Result<PathBuf> {
    if path.is_absolute() {
        return Ok(path.to_path_buf());
    }
    Ok(std::env::current_dir()?.join(path))
}
