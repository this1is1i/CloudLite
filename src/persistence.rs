use anyhow::Context;
use std::{fs, io::Write, path::Path};

fn atomic_bytes(path: &Path, data: &[u8]) -> anyhow::Result<()> {
    let parent = path.parent().context("文件缺少父目录")?;
    fs::create_dir_all(parent)?;
    let mut temp = tempfile::NamedTempFile::new_in(parent)?;
    temp.write_all(data)?;
    temp.as_file().sync_all()?;
    temp.persist(path).map_err(|e| e.error)?;
    Ok(())
}
pub fn save_bytes(path: &Path, data: &[u8]) -> anyhow::Result<()> {
    atomic_bytes(path, data)
}
pub fn save_json(path: &Path, value: &serde_json::Value) -> anyhow::Result<()> {
    if let Ok(old) = fs::read(path) {
        // Preserve the last valid generation; never replace it with a corrupt current file.
        if serde_json::from_slice::<serde_json::Value>(&old).is_ok() {
            atomic_bytes(&path.with_extension("json.bak"), &old)?;
        }
    }
    atomic_bytes(path, &serde_json::to_vec_pretty(value)?)
}
pub fn load_json(path: &Path) -> anyhow::Result<serde_json::Value> {
    fs::read(path)
        .map_err(anyhow::Error::from)
        .and_then(|d| Ok(serde_json::from_slice(&d)?))
        .or_else(|_| {
            Ok(serde_json::from_slice(&fs::read(
                path.with_extension("json.bak"),
            )?)?)
        })
}
