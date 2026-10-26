use std::fs;
use std::path::PathBuf;

pub fn create_dir_if_does_not_exist(path: PathBuf) -> anyhow::Result<PathBuf> {
    if !path.exists() {
        fs::create_dir(&path)?;
    }

    Ok(path)
}

pub fn subtract_project_root(path: &PathBuf) -> String {
    let current_dir = format!("{}/", env!("CARGO_MANIFEST_DIR"));
    path.to_string_lossy().replace(&current_dir, "")
}