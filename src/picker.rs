use anyhow::{ensure, Context};
use std::path::PathBuf;

pub fn decode_result(output: &[u8]) -> anyhow::Result<Option<PathBuf>> {
    ensure!(output.len() <= 65536, "目录选择结果过大");
    let value: serde_json::Value =
        serde_json::from_slice(output).context("目录选择器未返回有效结果")?;
    if let Some(error) = value["error"].as_str() {
        anyhow::bail!("{error}");
    }
    let value = value.get("path").context("目录选择器缺少路径字段")?;
    if value.is_null() {
        return Ok(None);
    }
    let path = PathBuf::from(value.as_str().context("目录路径无效")?);
    ensure!(
        path.is_absolute() && path.is_dir(),
        "选择的音乐目录不存在或不是绝对路径"
    );
    Ok(Some(path))
}
