mod credential_store;
mod provider_registry;
mod settings_store;

use parking_lot::Mutex;
use reqwest::Url;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Child;
use std::process::Command;
use std::sync::Arc;
use std::time::Duration;
use std::time::Instant;
use tauri::{Runtime, State};

#[cfg(test)]
use credential_store::InMemoryCredentialStore;
use credential_store::{CredentialStore, WindowsCredentialStore};
pub(crate) use provider_registry::{default_model_for_endpoint, provider_secret_key};
pub(crate) use settings_store::{
    default_settings, legacy_provider_api_key_from_settings, legacy_search_api_key_from_settings,
    read_settings, strip_secret_fields, write_settings,
};

const CREDENTIAL_SERVICE: &str = "aicleaner";
const SEARCH_SECRET_KEY: &str = "search:tavily:apiKey";
const STORAGE_LOCATION_FILE: &str = "storage-location.json";

#[derive(Clone, Debug)]
struct AppPaths {
    data_dir: PathBuf,
    settings_path: PathBuf,
    db_path: PathBuf,
}

impl AppPaths {
    fn from_data_dir(data_dir: PathBuf) -> Self {
        Self {
            settings_path: data_dir.join("settings.json"),
            db_path: data_dir.join("scan-cache.sqlite"),
            data_dir,
        }
    }
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct StorageLocationConfig {
    data_dir: Option<String>,
}

#[derive(Clone)]
pub struct AppState {
    pub base_data_dir: PathBuf,
    pub bootstrap_path: PathBuf,
    paths: Arc<Mutex<AppPaths>>,
    pub(crate) organize_tasks:
        Arc<Mutex<HashMap<String, Arc<crate::organizer_runtime::OrganizeTaskRuntime>>>>,
    pub(crate) tika_process: Arc<Mutex<Option<ManagedTikaProcess>>>,
    credential_store: Arc<dyn CredentialStore>,
}

pub(crate) struct ManagedTikaProcess {
    pub url: String,
    pub child: Child,
}

impl AppState {
    pub fn bootstrap(base_data_dir: PathBuf) -> Result<Self, String> {
        fs::create_dir_all(&base_data_dir).map_err(|e| e.to_string())?;
        let bootstrap_path = base_data_dir.join(STORAGE_LOCATION_FILE);
        let resolved_data_dir = resolve_storage_data_dir(&base_data_dir, &bootstrap_path);
        fs::create_dir_all(&resolved_data_dir).map_err(|e| e.to_string())?;
        let paths = AppPaths::from_data_dir(resolved_data_dir);
        if !paths.settings_path.exists() {
            fs::write(
                &paths.settings_path,
                serde_json::to_vec_pretty(&default_settings()).map_err(|e| e.to_string())?,
            )
            .map_err(|e| e.to_string())?;
        }
        Ok(Self {
            credential_store: Arc::new(WindowsCredentialStore::new(CREDENTIAL_SERVICE)),
            base_data_dir,
            bootstrap_path,
            paths: Arc::new(Mutex::new(paths)),
            organize_tasks: Arc::new(Mutex::new(HashMap::new())),
            tika_process: Arc::new(Mutex::new(None)),
        })
    }

    #[cfg(test)]
    fn with_store(settings_path: PathBuf, credential_store: Arc<dyn CredentialStore>) -> Self {
        let data_dir = settings_path
            .parent()
            .unwrap_or_else(|| Path::new("."))
            .to_path_buf();
        Self {
            base_data_dir: data_dir.clone(),
            bootstrap_path: data_dir.join(STORAGE_LOCATION_FILE),
            paths: Arc::new(Mutex::new(AppPaths {
                data_dir,
                settings_path,
                db_path: std::env::temp_dir().join("aicleaner-test.sqlite"),
            })),
            organize_tasks: Arc::new(Mutex::new(HashMap::new())),
            tika_process: Arc::new(Mutex::new(None)),
            credential_store,
        }
    }

    pub fn data_dir(&self) -> PathBuf {
        self.paths.lock().data_dir.clone()
    }

    pub fn settings_path(&self) -> PathBuf {
        self.paths.lock().settings_path.clone()
    }

    pub fn db_path(&self) -> PathBuf {
        self.paths.lock().db_path.clone()
    }

    pub fn uses_custom_data_dir(&self) -> bool {
        !same_path(&self.data_dir(), &self.base_data_dir)
    }

    fn set_data_dir(&self, data_dir: PathBuf) {
        *self.paths.lock() = AppPaths::from_data_dir(data_dir);
    }
}

include!("backend/storage_location.rs");

include!("backend/settings_credentials.rs");

include!("backend/commands.rs");

include!("backend/workflow_commands.rs");

#[cfg(test)]
include!("backend/tests.rs");
