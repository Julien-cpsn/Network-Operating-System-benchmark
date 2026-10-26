use std::path::{Path, PathBuf};
use anyhow::{anyhow, bail, Context};
use serde_json::Value;
use tokio::process::Command;
use tracing::debug;
use crate::QEMU_IMG_BINARY;

const TARGET: &str = "image";

/// Ensures each image exists and returns their absolute paths
/// (QEMU changes its working directory to "/" when it daemonizes).
pub fn resolve_images(image_path_list: &[PathBuf]) -> anyhow::Result<Vec<PathBuf>> {
    let mut resolved = Vec::with_capacity(image_path_list.len());

    for image_path in image_path_list {
        if !image_path.is_file() {
            bail!("Image \"{}\" does not exist or is not a file", image_path.display());
        }

        let absolute = image_path.canonicalize().with_context(|| format!("Could not resolve image \"{}\"", image_path.display()))?;
        debug!(target: TARGET, "Found image: {}", absolute.display());
        resolved.push(absolute);
    }

    Ok(resolved)
}

/// Format of a disk image, as detected by `qemu-img info` (e.g. "qcow2", "raw", "vmdk")
pub async fn image_format(image_path: &Path) -> anyhow::Result<String> {
    let output = Command::new(QEMU_IMG_BINARY.get().unwrap())
        .args(["info", "--output=json", "-U"])
        .arg(image_path)
        .output()
        .await
        .context("Could not run qemu-img")?;

    if !output.status.success() {
        bail!("qemu-img info failed on \"{}\": {}", image_path.display(), String::from_utf8_lossy(&output.stderr).trim());
    }

    let info: Value = serde_json::from_slice(&output.stdout)?;

    info.get("format")
        .and_then(Value::as_str)
        .map(str::to_string)
        .ok_or_else(|| anyhow!("qemu-img could not detect the format of \"{}\"", image_path.display()))
}

/// A qcow2 overlay backed by the untouched base image,
/// so every experiment starts from a pristine disk and the base image is never written.
pub async fn create_linked_clone(base_image: &Path, overlay: &Path) -> anyhow::Result<()> {
    let base_format = image_format(base_image).await?;

    let output = Command::new(QEMU_IMG_BINARY.get().unwrap())
        .args(["create", "-q", "-f", "qcow2", "-F", &base_format, "-b"])
        .arg(base_image)
        .arg(overlay)
        .output()
        .await
        .context("Could not run qemu-img")?;

    if !output.status.success() {
        bail!("qemu-img create failed for \"{}\": {}", overlay.display(), String::from_utf8_lossy(&output.stderr).trim());
    }

    debug!(target: TARGET, "Linked clone {} -> {} ({})", overlay.display(), base_image.display(), base_format);

    Ok(())
}
