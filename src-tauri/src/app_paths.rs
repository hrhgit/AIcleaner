use serde::{Deserialize, Serialize};
use std::fs;
use std::path::{Path, PathBuf};

pub const DEFAULT_DATA_DIR: &str = "E:/Cache/AIcleaner";
pub const STORAGE_LOCATION_FILE: &str = "storage-location.json";
pub const APP_IDENTIFIER: &str = "com.hrhgit.aicleaner";
pub const APP_LOG_FILE_STEM: &str = "app";

#[derive(Clone, Debug)]
pub struct AppPaths {
    pub data_dir: PathBuf,
    pub settings_path: PathBuf,
    pub db_path: PathBuf,
}

impl AppPaths {
    pub fn from_data_dir(data_dir: PathBuf) -> Self {
        Self {
            settings_path: data_dir.join("settings.json"),
            db_path: data_dir.join("scan-cache.sqlite"),
            data_dir,
        }
    }

    pub fn logs_dir(&self) -> PathBuf {
        self.data_dir.join("logs")
    }
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct StorageLocationConfig {
    pub data_dir: Option<String>,
}

pub fn resolve_storage_data_dir(base_data_dir: &Path, bootstrap_path: &Path) -> PathBuf {
    fs::read_to_string(bootstrap_path)
        .ok()
        .and_then(|raw| serde_json::from_str::<StorageLocationConfig>(&raw).ok())
        .and_then(|config| config.data_dir)
        .map(|value| PathBuf::from(value.trim()))
        .filter(|path| !path.as_os_str().is_empty())
        .unwrap_or_else(|| base_data_dir.to_path_buf())
}

pub fn default_data_dir() -> PathBuf {
    PathBuf::from(DEFAULT_DATA_DIR)
}

#[cfg(target_os = "windows")]
pub fn default_legacy_app_log_dir() -> Option<PathBuf> {
    std::env::var_os("LOCALAPPDATA")
        .map(PathBuf::from)
        .map(|base| base.join(APP_IDENTIFIER).join("logs"))
}

#[cfg(not(target_os = "windows"))]
pub fn default_legacy_app_log_dir() -> Option<PathBuf> {
    None
}
