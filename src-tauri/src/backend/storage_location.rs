fn resolve_storage_data_dir(base_data_dir: &Path, bootstrap_path: &Path) -> PathBuf {
    fs::read_to_string(bootstrap_path)
        .ok()
        .and_then(|raw| serde_json::from_str::<StorageLocationConfig>(&raw).ok())
        .and_then(|config| config.data_dir)
        .map(|value| PathBuf::from(value.trim()))
        .filter(|path| !path.as_os_str().is_empty())
        .unwrap_or_else(|| base_data_dir.to_path_buf())
}

fn write_storage_location_config(
    base_data_dir: &Path,
    bootstrap_path: &Path,
    active_data_dir: &Path,
) -> Result<(), String> {
    fs::create_dir_all(base_data_dir).map_err(|e| e.to_string())?;
    if same_path(active_data_dir, base_data_dir) {
        if bootstrap_path.exists() {
            fs::remove_file(bootstrap_path).map_err(|e| e.to_string())?;
        }
        return Ok(());
    }
    let payload = StorageLocationConfig {
        data_dir: Some(active_data_dir.to_string_lossy().to_string()),
    };
    fs::write(
        bootstrap_path,
        serde_json::to_vec_pretty(&payload).map_err(|e| e.to_string())?,
    )
    .map_err(|e| e.to_string())
}

fn same_path(left: &Path, right: &Path) -> bool {
    crate::persist::normalize_root_path(&left.to_string_lossy())
        == crate::persist::normalize_root_path(&right.to_string_lossy())
}

fn is_same_or_descendant_path(candidate: &Path, parent: &Path) -> bool {
    let candidate = crate::persist::normalize_root_path(&candidate.to_string_lossy());
    let parent = crate::persist::normalize_root_path(&parent.to_string_lossy());
    if candidate == parent {
        return true;
    }
    let Some(stripped) = candidate.strip_prefix(&parent) else {
        return false;
    };
    stripped.starts_with('\\')
}

fn validate_data_dir_target(current_data_dir: &Path, target_data_dir: &Path) -> Result<(), String> {
    if target_data_dir.as_os_str().is_empty() {
        return Err("Missing target data directory.".to_string());
    }
    if same_path(current_data_dir, target_data_dir) {
        return Err("The selected directory is already the current cache location.".to_string());
    }
    if is_same_or_descendant_path(target_data_dir, current_data_dir)
        || is_same_or_descendant_path(current_data_dir, target_data_dir)
    {
        return Err(
            "The new cache directory cannot be the current directory or its parent/child."
                .to_string(),
        );
    }
    Ok(())
}

fn copy_dir_contents_recursive(
    source_dir: &Path,
    target_dir: &Path,
    skip_paths: &HashSet<String>,
) -> Result<(), String> {
    fs::create_dir_all(target_dir).map_err(|e| e.to_string())?;
    for entry in fs::read_dir(source_dir).map_err(|e| e.to_string())? {
        let entry = entry.map_err(|e| e.to_string())?;
        let source_path = entry.path();
        let source_key = crate::persist::normalize_root_path(&source_path.to_string_lossy());
        if skip_paths.contains(&source_key) {
            continue;
        }
        let target_path = target_dir.join(entry.file_name());
        let file_type = entry.file_type().map_err(|e| e.to_string())?;
        if file_type.is_dir() {
            copy_dir_contents_recursive(&source_path, &target_path, skip_paths)?;
        } else if file_type.is_file() {
            if target_path.exists() {
                return Err(format!(
                    "Target already contains {}.",
                    target_path.to_string_lossy()
                ));
            }
            fs::copy(&source_path, &target_path).map_err(|e| e.to_string())?;
        }
    }
    Ok(())
}

fn remove_dir_contents_recursive(
    source_dir: &Path,
    skip_paths: &HashSet<String>,
) -> Result<(), String> {
    if !source_dir.exists() {
        return Ok(());
    }
    for entry in fs::read_dir(source_dir).map_err(|e| e.to_string())? {
        let entry = entry.map_err(|e| e.to_string())?;
        let source_path = entry.path();
        let source_key = crate::persist::normalize_root_path(&source_path.to_string_lossy());
        if skip_paths.contains(&source_key) {
            continue;
        }
        let file_type = entry.file_type().map_err(|e| e.to_string())?;
        if file_type.is_dir() {
            remove_dir_contents_recursive(&source_path, skip_paths)?;
            let _ = fs::remove_dir(&source_path);
        } else if file_type.is_file() {
            fs::remove_file(&source_path).map_err(|e| e.to_string())?;
        }
    }
    Ok(())
}

#[derive(Clone, Debug, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct TokenUsage {
    pub prompt: u64,
    pub completion: u64,
    pub total: u64,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct OrganizeSnapshot {
    pub id: String,
    pub status: String,
    pub error: Option<String>,
    pub root_path: String,
    pub recursive: bool,
    pub excluded_patterns: Vec<String>,
    pub batch_size: u32,
    #[serde(default = "default_organize_summary_strategy", alias = "summaryMode")]
    pub summary_strategy: String,
    pub max_cluster_depth: Option<u32>,
    pub use_web_search: bool,
    pub web_search_enabled: bool,
    pub selected_model: String,
    pub selected_models: Value,
    pub selected_providers: Value,
    pub supports_multimodal: bool,
    pub tree: Value,
    pub tree_version: u64,
    pub total_files: u64,
    pub processed_files: u64,
    pub total_batches: u64,
    pub processed_batches: u64,
    pub token_usage: TokenUsage,
    #[serde(default)]
    pub results: Vec<Value>,
    #[serde(default)]
    pub preview: Vec<Value>,
    pub created_at: String,
    pub completed_at: Option<String>,
    pub job_id: Option<String>,
}

pub fn default_organize_summary_strategy() -> String {
    "filename_only".to_string()
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CredentialsSaveInput {
    provider_secrets: Option<HashMap<String, String>>,
    search_api_key: Option<String>,
}

