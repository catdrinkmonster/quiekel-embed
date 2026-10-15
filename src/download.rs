//! The model's files: fetched once into the Hugging Face cache (shared with other apps), as plain
//! files in the model's folder.

use std::io::Write;
use std::path::Path;
use std::time::Duration;

use anyhow::{Context, Result};

/// Where Windows allows links (Developer Mode, an administrator), the Hugging Face cache keeps
/// each file once, elsewhere, and links to it from the model's folder. ONNX Runtime refuses
/// weights outside the model's folder, so such links become hard links (the same file, no space
/// taken), or copies where that fails.
pub fn as_real_files(folder: &Path, names: &[String]) {
    for name in names {
        let path = folder.join(name);
        let Ok(meta) = std::fs::symlink_metadata(&path) else { continue };
        if !meta.file_type().is_symlink() {
            continue;
        }
        let Ok(target) = std::fs::canonicalize(&path) else { continue };
        let tmp = path.with_file_name(format!(
            "{}.{}.tmp",
            path.file_name().unwrap_or_default().to_string_lossy(),
            std::process::id()
        ));
        let made = std::fs::hard_link(&target, &tmp).or_else(|_| std::fs::copy(&target, &tmp).map(|_| ()));
        if made.is_ok() {
            let _ = std::fs::rename(&tmp, &path);
        }
        let _ = std::fs::remove_file(&tmp);
    }
}

/// Fetch what's missing of the model's files for a graphics card or the processor.
pub fn model(gpu: bool) -> Result<()> {
    let folder = crate::onnx::snapshot();
    let missing: Vec<String> = crate::onnx::files(gpu).into_iter().filter(|f| !folder.join(f).is_file()).collect();
    for name in missing {
        let url = format!("https://huggingface.co/{}/resolve/{}/{name}", crate::onnx::REPO, crate::onnx::REVISION);
        log::info!("Downloading {name}");
        fetch(&url, &folder.join(&name)).with_context(|| format!("downloading {name}"))?;
    }
    Ok(())
}

/// One file, to a temporary name first: a broken download never looks like a finished one.
fn fetch(url: &str, target: &Path) -> Result<()> {
    if let Some(dir) = target.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let tmp = target.with_file_name(format!(
        "{}.{}.part",
        target.file_name().unwrap_or_default().to_string_lossy(),
        std::process::id()
    ));
    let agent: ureq::Agent = ureq::Agent::config_builder()
        .timeout_connect(Some(Duration::from_secs(30)))
        .timeout_recv_response(Some(Duration::from_secs(60)))
        .build()
        .into();
    let mut reply = agent.get(url).header("User-Agent", &format!("quiekel-embed/{}", env!("CARGO_PKG_VERSION"))).call()?;
    let mut body = reply.body_mut().with_config().limit(u64::MAX).reader();
    let result = (|| -> Result<()> {
        let mut file = std::fs::File::create(&tmp)?;
        std::io::copy(&mut body, &mut file)?;
        file.flush()?;
        file.sync_all()?;
        Ok(())
    })();
    if let Err(e) = result {
        let _ = std::fs::remove_file(&tmp);
        return Err(e);
    }
    std::fs::rename(&tmp, target)?;
    Ok(())
}
