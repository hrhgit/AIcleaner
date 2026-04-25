#[cfg(test)]
mod tests {
    use super::*;
    use uuid::Uuid;

    fn temp_settings_path() -> PathBuf {
        std::env::temp_dir().join(format!("aicleaner-settings-{}.json", Uuid::new_v4()))
    }

    #[test]
    fn partial_save_preserves_credentials_meta() {
        let path = temp_settings_path();
        let seeded = json!({
            "credentialsMeta": {
                "providers": {
                    "https://api.deepseek.com": true
                },
                "searchApi": true
            }
        });
        write_settings(&path, &seeded).expect("seed settings");

        let patch = json!({
            "contentExtraction": {
                "tika": {
                    "url": "http://127.0.0.1:9999"
                }
            }
        });
        write_settings(&path, &patch).expect("partial save");

        let saved = read_settings(&path);
        assert_eq!(
            saved["credentialsMeta"]["providers"]["https://api.deepseek.com"],
            Value::Bool(true)
        );
        assert_eq!(saved["credentialsMeta"]["searchApi"], Value::Bool(true));

        let _ = fs::remove_file(path);
    }

    #[test]
    fn partial_save_preserves_custom_provider_configs() {
        let path = temp_settings_path();
        let custom_endpoint = "https://example.com/openai";
        let seeded = json!({
            "defaultProviderEndpoint": custom_endpoint,
            "providerConfigs": {
                custom_endpoint: {
                    "name": "Custom",
                    "endpoint": custom_endpoint,
                    "model": "custom-model"
                }
            }
        });
        write_settings(&path, &seeded).expect("seed provider config");

        let patch = json!({
            "contentExtraction": {
                "tika": {
                    "enabled": false
                }
            }
        });
        write_settings(&path, &patch).expect("partial save");

        let saved = read_settings(&path);
        assert_eq!(
            saved["defaultProviderEndpoint"],
            Value::String(custom_endpoint.to_string())
        );
        assert_eq!(
            saved["providerConfigs"][custom_endpoint]["model"],
            Value::String("custom-model".to_string())
        );

        let _ = fs::remove_file(path);
    }

    #[test]
    fn read_settings_migrates_legacy_provider_and_search_fields() {
        let path = temp_settings_path();
        let legacy = json!({
            "apiEndpoint": "https://api.deepseek.com",
            "model": "deepseek-reasoner",
            "enableWebSearch": true,
            "enableWebSearchOrganizer": true,
            "providerConfigs": {
                "https://api.deepseek.com": {
                    "apiKey": "sk-legacy-inline"
                }
            },
            "tavilyApiKey": "tvly-legacy-inline"
        });
        fs::write(
            &path,
            serde_json::to_string_pretty(&legacy).expect("serialize legacy settings"),
        )
        .expect("write legacy settings");

        let saved = read_settings(&path);

        assert_eq!(
            saved["defaultProviderEndpoint"],
            Value::String("https://api.deepseek.com".to_string())
        );
        assert_eq!(
            saved["providerConfigs"]["https://api.deepseek.com"]["model"],
            Value::String("deepseek-reasoner".to_string())
        );
        assert_eq!(saved["searchApi"]["enabled"], Value::Bool(true));
        assert_eq!(saved["searchApi"]["scopes"]["classify"], Value::Bool(true));
        assert_eq!(saved["searchApi"]["scopes"]["organizer"], Value::Bool(true));
        assert!(saved.get("apiEndpoint").is_none());
        assert!(saved.get("model").is_none());
        assert!(saved.get("enableWebSearch").is_none());
        assert!(saved.get("enableWebSearchOrganizer").is_none());
        assert!(saved.get("tavilyApiKey").is_none());

        let _ = fs::remove_file(path);
    }

    #[test]
    fn read_settings_upgrades_legacy_tika_defaults_for_ui_consistency() {
        let path = temp_settings_path();
        let legacy = json!({
            "contentExtraction": {
                "tika": {
                    "enabled": false,
                    "autoStart": false,
                    "url": "http://127.0.0.1:9998",
                    "jarPath": ""
                }
            }
        });
        fs::write(
            &path,
            serde_json::to_string_pretty(&legacy).expect("serialize legacy tika settings"),
        )
        .expect("write legacy tika settings");

        let saved = read_settings(&path);

        assert_eq!(
            saved["contentExtraction"]["tika"]["enabled"],
            Value::Bool(true)
        );
        assert_eq!(
            saved["contentExtraction"]["tika"]["autoStart"],
            Value::Bool(true)
        );

        let _ = fs::remove_file(path);
    }

    #[test]
    fn organize_snapshot_defaults_summary_strategy_when_missing() {
        let snapshot: OrganizeSnapshot = serde_json::from_value(json!({
            "id": "task_legacy",
            "status": "completed",
            "error": null,
            "rootPath": "C:\\root",
            "recursive": true,
            "excludedPatterns": [],
            "batchSize": 20,
            "maxClusterDepth": null,
            "useWebSearch": false,
            "webSearchEnabled": false,
            "selectedModel": "deepseek-chat",
            "selectedModels": {},
            "selectedProviders": {},
            "supportsMultimodal": false,
            "tree": {},
            "treeVersion": 0,
            "totalFiles": 0,
            "processedFiles": 0,
            "totalBatches": 0,
            "processedBatches": 0,
            "tokenUsage": { "prompt": 0, "completion": 0, "total": 0 },
            "results": [],
            "preview": [],
            "createdAt": "2026-03-28T00:00:00Z",
            "completedAt": null,
            "jobId": null
        }))
        .expect("deserialize legacy snapshot");

        assert_eq!(
            snapshot.summary_strategy,
            default_organize_summary_strategy()
        );
    }

    #[test]
    fn resolve_credentials_fall_back_to_legacy_inline_values() {
        let path = temp_settings_path();
        let settings = json!({
            "defaultProviderEndpoint": "https://api.deepseek.com",
            "providerConfigs": {
                "https://api.deepseek.com": {
                    "endpoint": "https://api.deepseek.com",
                    "model": "deepseek-chat",
                    "apiKey": "sk-inline"
                }
            },
            "searchApi": {
                "provider": "tavily",
                "enabled": true,
                "apiKey": "tvly-inline",
                "scopes": {
                    "scan": true,
                    "classify": true,
                    "organizer": true
                }
            }
        });
        fs::write(
            &path,
            serde_json::to_string_pretty(&settings).expect("serialize settings"),
        )
        .expect("write settings");

        let state =
            AppState::with_store(path.clone(), Arc::new(InMemoryCredentialStore::default()));
        let raw = read_settings(&state.settings_path());
        let status = build_credentials_status_value(&state, &raw);

        assert_eq!(
            resolve_provider_api_key(&state, "https://api.deepseek.com")
                .expect("fallback provider secret"),
            "sk-inline".to_string()
        );
        assert_eq!(
            resolve_search_api_key(&state).expect("fallback search secret"),
            "tvly-inline".to_string()
        );
        assert_eq!(
            status["providerHasApiKey"]["https://api.deepseek.com"],
            Value::Bool(true)
        );
        assert_eq!(status["searchApiHasKey"], Value::Bool(true));

        let _ = fs::remove_file(path);
    }

    fn temp_app_state() -> (AppState, PathBuf) {
        let path = temp_settings_path();
        write_settings(&path, &default_settings()).expect("seed default settings");
        let state =
            AppState::with_store(path.clone(), Arc::new(InMemoryCredentialStore::default()));
        (state, path)
    }

    #[test]
    fn credentials_meta_reflects_saved_values() {
        let (state, path) = temp_app_state();
        let payload = CredentialsSaveInput {
            provider_secrets: Some(HashMap::from([(
                "https://api.openai.com/v1".to_string(),
                "sk-test".to_string(),
            )])),
            search_api_key: Some("tvly-test".to_string()),
        };

        let settings = read_settings(&state.settings_path());
        let mut provider_meta = HashMap::new();
        provider_meta.insert("https://api.openai.com/v1".to_string(), true);
        state
            .credential_store
            .set_many(&[
                (
                    provider_secret_key("https://api.openai.com/v1"),
                    "sk-test".to_string(),
                ),
                (SEARCH_SECRET_KEY.to_string(), "tvly-test".to_string()),
            ])
            .expect("save credentials");
        let saved = update_credentials_meta_in_settings(&state, &provider_meta, true)
            .expect("update credentials meta");
        let status = build_credentials_status_value(&state, &settings);

        assert_eq!(
            saved["credentialsMeta"]["providers"]["https://api.openai.com/v1"],
            Value::Bool(true)
        );
        assert_eq!(saved["credentialsMeta"]["searchApi"], Value::Bool(true));
        assert_eq!(
            status["providerHasApiKey"]["https://api.openai.com/v1"],
            Value::Bool(true)
        );
        assert_eq!(status["searchApiHasKey"], Value::Bool(true));
        assert_eq!(payload.search_api_key, Some("tvly-test".to_string()));

        let _ = fs::remove_file(path);
    }

    #[test]
    fn credentials_meta_clears_when_empty_values_saved() {
        let (state, path) = temp_app_state();
        state
            .credential_store
            .set_many(&[
                (
                    provider_secret_key("https://api.openai.com/v1"),
                    "sk-test".to_string(),
                ),
                (SEARCH_SECRET_KEY.to_string(), "tvly-test".to_string()),
            ])
            .expect("seed credentials");

        let provider_meta = HashMap::from([("https://api.openai.com/v1".to_string(), false)]);
        let saved = update_credentials_meta_in_settings(&state, &provider_meta, false)
            .expect("update credentials meta");
        state
            .credential_store
            .set_many(&[
                (
                    provider_secret_key("https://api.openai.com/v1"),
                    String::new(),
                ),
                (SEARCH_SECRET_KEY.to_string(), String::new()),
            ])
            .expect("clear credentials");

        let status = build_credentials_status_value(&state, &read_settings(&state.settings_path()));

        assert_eq!(
            saved["credentialsMeta"]["providers"]["https://api.openai.com/v1"],
            Value::Bool(false)
        );
        assert_eq!(saved["credentialsMeta"]["searchApi"], Value::Bool(false));
        assert_eq!(
            status["providerHasApiKey"]["https://api.openai.com/v1"],
            Value::Bool(false)
        );
        assert_eq!(status["searchApiHasKey"], Value::Bool(false));

        let _ = fs::remove_file(path);
    }

    #[test]
    fn credentials_save_preserves_existing_values_for_omitted_fields() {
        let (state, path) = temp_app_state();
        state
            .credential_store
            .set_many(&[
                (
                    provider_secret_key("https://api.openai.com/v1"),
                    "sk-existing".to_string(),
                ),
                (SEARCH_SECRET_KEY.to_string(), "tvly-existing".to_string()),
            ])
            .expect("seed credentials");

        save_credentials_internal(
            &state,
            CredentialsSaveInput {
                provider_secrets: Some(HashMap::new()),
                search_api_key: None,
            },
        )
        .expect("save without touching secrets");

        assert_eq!(
            state
                .credential_store
                .get(&provider_secret_key("https://api.openai.com/v1"))
                .expect("read provider credential"),
            Some("sk-existing".to_string())
        );
        assert_eq!(
            state
                .credential_store
                .get(SEARCH_SECRET_KEY)
                .expect("read search credential"),
            Some("tvly-existing".to_string())
        );

        let saved = read_settings(&state.settings_path());
        assert_eq!(
            saved["credentialsMeta"]["providers"]["https://api.openai.com/v1"],
            Value::Bool(true)
        );
        assert_eq!(saved["credentialsMeta"]["searchApi"], Value::Bool(true));

        let _ = fs::remove_file(path);
    }

    #[test]
    fn credentials_save_updates_only_explicitly_touched_fields() {
        let (state, path) = temp_app_state();
        state
            .credential_store
            .set_many(&[
                (
                    provider_secret_key("https://api.openai.com/v1"),
                    "sk-openai".to_string(),
                ),
                (
                    provider_secret_key("https://api.deepseek.com"),
                    "sk-deepseek".to_string(),
                ),
                (SEARCH_SECRET_KEY.to_string(), "tvly-existing".to_string()),
            ])
            .expect("seed credentials");

        save_credentials_internal(
            &state,
            CredentialsSaveInput {
                provider_secrets: Some(HashMap::from([(
                    "https://api.openai.com/v1".to_string(),
                    String::new(),
                )])),
                search_api_key: Some(String::new()),
            },
        )
        .expect("save touched secrets");

        assert_eq!(
            state
                .credential_store
                .get(&provider_secret_key("https://api.openai.com/v1"))
                .expect("read cleared provider credential"),
            None
        );
        assert_eq!(
            state
                .credential_store
                .get(&provider_secret_key("https://api.deepseek.com"))
                .expect("read untouched provider credential"),
            Some("sk-deepseek".to_string())
        );
        assert_eq!(
            state
                .credential_store
                .get(SEARCH_SECRET_KEY)
                .expect("read cleared search credential"),
            None
        );

        let saved = read_settings(&state.settings_path());
        assert_eq!(
            saved["credentialsMeta"]["providers"]["https://api.openai.com/v1"],
            Value::Bool(false)
        );
        assert_eq!(
            saved["credentialsMeta"]["providers"]["https://api.deepseek.com"],
            Value::Bool(true)
        );
        assert_eq!(saved["credentialsMeta"]["searchApi"], Value::Bool(false));

        let _ = fs::remove_file(path);
    }

    #[test]
    fn migrate_data_dir_moves_settings_and_database() {
        let base_dir = std::env::temp_dir().join(format!("aicleaner-data-root-{}", Uuid::new_v4()));
        let target_dir =
            std::env::temp_dir().join(format!("aicleaner-data-target-{}", Uuid::new_v4()));
        fs::create_dir_all(&base_dir).expect("create base dir");

        let state = AppState::bootstrap(base_dir.clone()).expect("bootstrap state");
        write_settings(
            &state.settings_path(),
            &json!({
                "contentExtraction": {
                    "tika": {
                        "enabled": false
                    }
                }
            }),
        )
        .expect("write settings");
        fs::write(state.data_dir().join("other-file.txt"), b"other").expect("write other file");

        let response =
            migrate_data_dir(&state, &target_dir.to_string_lossy()).expect("migrate data dir");

        assert!(same_path(&state.data_dir(), &target_dir));
        assert!(state.settings_path().exists());
        assert!(state.db_path().exists());
        // Only settings.json and scan-cache.sqlite are migrated, not other files
        assert!(target_dir.join("settings.json").exists());
        assert!(target_dir.join("scan-cache.sqlite").exists());
        assert!(!target_dir.join("other-file.txt").exists());
        assert_eq!(
            response["settings"]["storage"]["dataDir"],
            Value::String(target_dir.to_string_lossy().to_string())
        );
        assert_eq!(
            resolve_storage_data_dir(&base_dir, &state.bootstrap_path),
            target_dir
        );
        assert!(!base_dir.join("settings.json").exists());
        assert!(!base_dir.join("scan-cache.sqlite").exists());

        let _ = fs::remove_dir_all(&target_dir);
        let _ = fs::remove_file(&state.bootstrap_path);
        let _ = fs::remove_dir_all(&base_dir);
    }
}
