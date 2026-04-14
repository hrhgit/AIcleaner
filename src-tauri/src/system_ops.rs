use base64::Engine;
use serde_json::{json, Value};
use std::path::Path;
use std::process::Command;

fn encode_path(path: &Path) -> String {
    base64::engine::general_purpose::STANDARD.encode(path.to_string_lossy().as_bytes())
}

fn powershell_recycle_script(path: &Path) -> String {
    let encoded = encode_path(path);
    format!(
        r#"$ErrorActionPreference = 'Stop'
Add-Type -AssemblyName Microsoft.VisualBasic
$path = [System.Text.Encoding]::UTF8.GetString([System.Convert]::FromBase64String('{encoded}'))
if (-not (Test-Path -LiteralPath $path)) {{
  throw 'path_not_found'
}}
if (Test-Path -LiteralPath $path -PathType Container) {{
  [Microsoft.VisualBasic.FileIO.FileSystem]::DeleteDirectory(
    $path,
    [Microsoft.VisualBasic.FileIO.UIOption]::OnlyErrorDialogs,
    [Microsoft.VisualBasic.FileIO.RecycleOption]::SendToRecycleBin
  )
}} else {{
  [Microsoft.VisualBasic.FileIO.FileSystem]::DeleteFile(
    $path,
    [Microsoft.VisualBasic.FileIO.UIOption]::OnlyErrorDialogs,
    [Microsoft.VisualBasic.FileIO.RecycleOption]::SendToRecycleBin
  )
}}"#
    )
}

pub fn move_to_recycle_bin(path: &Path) -> Result<(), String> {
    if !cfg!(windows) {
        return Err("Recycle bin is only supported on Windows.".to_string());
    }
    let output = Command::new("powershell.exe")
        .arg("-NoProfile")
        .arg("-NonInteractive")
        .arg("-ExecutionPolicy")
        .arg("Bypass")
        .arg("-Command")
        .arg(powershell_recycle_script(path))
        .output()
        .map_err(|e| e.to_string())?;
    if output.status.success() {
        Ok(())
    } else {
        let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
        let stdout = String::from_utf8_lossy(&output.stdout).trim().to_string();
        let detail = if !stderr.is_empty() {
            stderr
        } else if !stdout.is_empty() {
            stdout
        } else {
            format!("powershell exited with {}", output.status)
        };
        Err(detail)
    }
}

pub fn recycle_many(paths: &[String]) -> Vec<Value> {
    paths
        .iter()
        .map(|raw| {
            let path = Path::new(raw);
            if !path.exists() {
                return json!({
                    "path": raw,
                    "status": "failed",
                    "error": "path_not_found"
                });
            }
            match move_to_recycle_bin(path) {
                Ok(()) => json!({
                    "path": raw,
                    "status": "recycled",
                    "error": Value::Null
                }),
                Err(err) => json!({
                    "path": raw,
                    "status": "failed",
                    "error": err
                }),
            }
        })
        .collect()
}
