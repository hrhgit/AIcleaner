fn summarize_directory_tree(
    path: &Path,
    stop: &AtomicBool,
) -> (u64, u64, u64, HashMap<String, u64>, u32) {
    let mut total_size = 0_u64;
    let mut file_count = 0_u64;
    let mut dir_count = 0_u64;
    let mut ext_counts = HashMap::new();
    let mut max_depth = 0_u32;
    for entry in WalkDir::new(path)
        .min_depth(1)
        .into_iter()
        .filter_map(Result::ok)
    {
        if stop.load(Ordering::Relaxed) {
            break;
        }
        let entry_path = entry.path();
        max_depth = max_depth.max(entry.depth() as u32);
        if entry.file_type().is_dir() {
            dir_count = dir_count.saturating_add(1);
            continue;
        }
        if !entry.file_type().is_file() {
            continue;
        }
        file_count = file_count.saturating_add(1);
        if let Ok(meta) = entry.metadata() {
            total_size = total_size.saturating_add(meta.len());
        }
        let key = extension_key(entry_path);
        *ext_counts.entry(key).or_insert(0) += 1;
    }
    (total_size, file_count, dir_count, ext_counts, max_depth)
}

fn is_collection_root(path: &Path, excluded: &[String], stop: &AtomicBool) -> bool {
    let entries = match fs::read_dir(path) {
        Ok(entries) => entries,
        Err(_) => return false,
    };
    let root_name = path
        .file_name()
        .and_then(|x| x.to_str())
        .unwrap_or("")
        .to_ascii_lowercase();
    let has_download_name = DOWNLOAD_ROOT_TOKENS
        .iter()
        .any(|token| root_name.contains(token));
    let mut direct_file_count = 0_u64;
    let mut direct_dir_count = 0_u64;
    let mut file_families = HashSet::<String>::new();

    for entry in entries.filter_map(Result::ok) {
        if stop.load(Ordering::Relaxed) {
            break;
        }
        let name = entry.file_name().to_string_lossy().to_string();
        if should_exclude(&name, excluded) {
            continue;
        }
        let entry_path = entry.path();
        if entry_path.is_dir() {
            direct_dir_count = direct_dir_count.saturating_add(1);
            continue;
        }
        if entry_path.is_file() {
            direct_file_count = direct_file_count.saturating_add(1);
            file_families
                .insert(classify_extension_family(&extension_key(&entry_path)).to_string());
        }
    }

    (has_download_name && direct_dir_count >= 4)
        || (direct_dir_count >= 8 && direct_file_count >= 6 && file_families.len() >= 4)
        || (direct_dir_count >= 12 && file_families.len() >= 3)
}

fn evaluate_directory_assessment(
    path: &Path,
    stop: &AtomicBool,
    prefer_whole: bool,
) -> Option<DirectoryAssessment> {
    let mut marker_files = Vec::new();
    let mut evidence = Vec::new();
    let mut app_signals = Vec::new();
    let mut fragmentation_warnings = Vec::new();
    let mut top_level_entries = Vec::new();
    let mut direct_family_counts = HashMap::<String, u64>::new();
    let mut direct_file_names = Vec::new();
    let mut direct_dir_names = Vec::new();
    let mut direct_child_dirs = Vec::<PathBuf>::new();
    let mut has_readme = false;
    let mut has_src = false;
    let mut has_bin = false;
    let mut has_lib = false;
    let mut has_resources = false;
    let mut has_docs = false;
    let mut has_images = false;
    let mut has_labels = false;
    let mut has_annotations = false;
    let mut has_train = false;
    let mut has_val = false;
    let mut has_test = false;
    let mut has_mods = false;
    let mut junk_named_dirs = 0_u64;
    let mut direct_exe_count = 0_u32;
    let mut direct_dll_count = 0_u32;
    let mut direct_archive_count = 0_u32;
    let mut direct_font_count = 0_u32;
    let mut direct_text_count = 0_u32;
    let mut direct_image_count = 0_u32;
    let mut direct_video_count = 0_u32;
    let mut direct_audio_count = 0_u32;
    let mut direct_runtime_count = 0_u32;
    let mut direct_config_count = 0_u32;
    let mut direct_script_count = 0_u32;
    let mut direct_json_count = 0_u32;
    let mut direct_pck_count = 0_u32;
    let mut direct_pak_count = 0_u32;
    let mut direct_bin_payload_count = 0_u32;
    let mut direct_cursor_count = 0_u32;
    let mut direct_inf_count = 0_u32;
    let mut metadata_marker_count = 0_u32;

    let entries = fs::read_dir(path).ok()?;
    for entry in entries.filter_map(Result::ok) {
        if stop.load(Ordering::Relaxed) {
            break;
        }
        let name = entry.file_name().to_string_lossy().to_string();
        let lower = name.to_ascii_lowercase();
        if top_level_entries.len() < 18 {
            top_level_entries.push(name.clone());
        }
        if PROJECT_MARKER_NAMES.iter().any(|marker| lower == *marker) {
            marker_files.push(name.clone());
        }

        let entry_path = entry.path();
        if entry_path.is_dir() {
            direct_dir_names.push(name.clone());
            direct_child_dirs.push(entry_path.clone());
            match lower.as_str() {
                "src" | "app" => has_src = true,
                "bin" => has_bin = true,
                "lib" => has_lib = true,
                "resources" | "resource" => has_resources = true,
                "docs" | "doc" => has_docs = true,
                "images" | "image" | "img" | "imgs" => has_images = true,
                "labels" => has_labels = true,
                "annotations" | "annotation" => has_annotations = true,
                "train" => has_train = true,
                "val" | "valid" | "validation" => has_val = true,
                "test" | "tests" => has_test = true,
                "mods" | "plugins" | "plugin" => has_mods = true,
                _ => {}
            }
            if JUNK_DIR_NAMES.iter().any(|value| lower == *value) {
                junk_named_dirs = junk_named_dirs.saturating_add(1);
            }
            continue;
        }

        direct_file_names.push(name.clone());
        if lower == "readme.md" || lower == "readme.txt" || lower == "readme" {
            has_readme = true;
        }
        if lower.ends_with(".exe") {
            direct_exe_count += 1;
        }
        if lower.ends_with(".dll") {
            direct_dll_count += 1;
        }
        if lower.ends_with(".json") {
            direct_json_count += 1;
        }
        if lower.ends_with(".pck") {
            direct_pck_count += 1;
        }
        if lower.ends_with(".pak") {
            direct_pak_count += 1;
        }
        if lower.ends_with(".bin") {
            direct_bin_payload_count += 1;
        }
        if lower.ends_with(".ani") || lower.ends_with(".cur") {
            direct_cursor_count += 1;
        }
        if lower.ends_with(".inf") {
            direct_inf_count += 1;
        }
        if lower.contains("manifest")
            || lower.contains("metadata")
            || lower.contains("catalog")
            || lower.contains("index")
        {
            metadata_marker_count += 1;
        }

        let family = classify_extension_family(&extension_key(&entry_path)).to_string();
        *direct_family_counts.entry(family.clone()).or_insert(0) += 1;
        match family.as_str() {
            "archive" => direct_archive_count += 1,
            "font" => direct_font_count += 1,
            "document" => direct_text_count += 1,
            "image" => direct_image_count += 1,
            "video" => direct_video_count += 1,
            "audio" => direct_audio_count += 1,
            "runtime" => direct_runtime_count += 1,
            "config" => direct_config_count += 1,
            "script" => direct_script_count += 1,
            _ => {}
        }
    }

    let (total_size, file_count, dir_count, ext_counts, max_depth) =
        summarize_directory_tree(path, stop);
    let dominant_extensions = format_ranked_entries(ext_counts.clone(), 8);
    let (name_families, max_family_count) = summarize_name_families(&direct_file_names, 5);
    let paired_sidecars = summarize_sidecars(&direct_file_names, 5);
    let root_bundle_key = canonical_bundle_key(
        path.file_name()
            .and_then(|x| x.to_str())
            .unwrap_or_default(),
    );
    let root_named_file_count = direct_file_names
        .iter()
        .filter(|name| matches_bundle_root(&root_bundle_key, name))
        .count() as u64;
    let root_named_binary_count = direct_file_names
        .iter()
        .filter(|name| {
            matches_bundle_root(&root_bundle_key, name)
                && matches!(
                    classify_extension_family(&extension_key(Path::new(name))),
                    "app" | "config"
                )
        })
        .count() as u64;
    let package_doc_count = direct_file_names
        .iter()
        .filter(|name| is_package_doc_name(name))
        .count() as u64;
    let multi_variant_app_bundle = root_named_binary_count >= 2 && direct_exe_count >= 2;
    let direct_file_count = direct_file_names.len() as u64;
    let direct_dir_count = direct_dir_names.len() as u64;
    let document_collection_share =
        (direct_text_count + direct_archive_count) as u64 * 100 / direct_file_count.max(1);
    let document_collection_layout = direct_text_count >= 8
        && document_collection_share >= 85
        && direct_exe_count == 0
        && direct_dll_count == 0
        && direct_image_count <= 3
        && direct_video_count == 0
        && direct_audio_count == 0
        && direct_runtime_count == 0
        && direct_script_count == 0;
    let dominant_extension_count = ext_counts.values().copied().max().unwrap_or(0);
    let dominant_share = if file_count == 0 {
        0.0
    } else {
        dominant_extension_count as f64 / file_count as f64
    };
    let naming_cohesion = if max_family_count >= 3 {
        "high".to_string()
    } else if max_family_count == 2 || dominant_share >= 0.55 {
        "medium".to_string()
    } else {
        "low".to_string()
    };

    let wrapper_target_path = if direct_dir_count == 1
        && ((direct_file_count == 0)
            || (direct_file_count <= 2
                && direct_file_names.iter().all(|name| {
                    let lower = name.to_ascii_lowercase();
                    WRAPPER_FILE_EXTS.iter().any(|ext| lower.ends_with(ext))
                })))
        && marker_files.is_empty()
        && direct_exe_count == 0
        && direct_dll_count == 0
        && direct_archive_count == 0
        && direct_font_count == 0
        && direct_script_count == 0
    {
        direct_child_dirs
            .first()
            .map(|child| child.to_string_lossy().to_string())
    } else {
        None
    };

    let runtime_like_only = direct_file_count > 0
        && (direct_runtime_count + direct_config_count) as u64 * 100 / direct_file_count >= 70
        && direct_exe_count == 0
        && direct_dll_count == 0
        && direct_text_count <= 1
        && direct_image_count == 0
        && direct_video_count == 0
        && direct_audio_count == 0
        && direct_font_count == 0
        && direct_script_count <= 1
        && marker_files.is_empty();
    let staging_junk = if runtime_like_only {
        junk_named_dirs == direct_dir_count || direct_dir_count == 0
    } else {
        false
    };
    let dll_only_directory = direct_dll_count >= 2
        && u64::from(direct_dll_count) == direct_file_count
        && direct_dir_count == 0
        && direct_exe_count == 0
        && direct_json_count == 0
        && direct_pck_count == 0
        && direct_config_count == 0
        && direct_text_count == 0
        && direct_image_count == 0
        && direct_video_count == 0
        && direct_audio_count == 0
        && direct_script_count == 0;

    let mut integrity_kind = "mixed".to_string();
    let mut score = 0_i32;
    let mut strong_anchor = false;

    if !marker_files.is_empty() {
        score += 38;
        strong_anchor = true;
        integrity_kind = "project".to_string();
        evidence.push(format!("markerFiles={}", marker_files.join(",")));
        app_signals.push("project_markers".to_string());
    }
    if has_readme {
        score += 8;
        evidence.push("readme_present".to_string());
        app_signals.push("readme_present".to_string());
    }
    if package_doc_count > 0 {
        score += (package_doc_count.min(3) as i32) * 3;
        evidence.push(format!("packageDocs={package_doc_count}"));
        app_signals.push(format!("package_docs:{package_doc_count}"));
    }
    if has_readme && has_src {
        score += 30;
        strong_anchor = true;
        integrity_kind = "project".to_string();
        evidence.push("readme+src".to_string());
        app_signals.push("readme+src".to_string());
    }
    if ((has_train && has_val) || (has_train && has_test) || (has_val && has_test))
        || ((has_images || has_docs) && (has_labels || has_annotations))
    {
        score += 32;
        strong_anchor = true;
        integrity_kind = "dataset_bundle".to_string();
        evidence.push("dataset_skeleton".to_string());
        app_signals.push("dataset_skeleton".to_string());
    }
    if direct_exe_count > 0
        && (direct_dll_count > 0
            || has_resources
            || has_bin
            || has_lib
            || direct_pak_count > 0
            || direct_bin_payload_count > 0)
    {
        score += 36;
        strong_anchor = true;
        integrity_kind = "app_bundle".to_string();
        evidence.push("exe+companions".to_string());
        app_signals.push(format!("exe:{direct_exe_count}"));
    } else if direct_dll_count > 0
        && (direct_json_count > 0
            || direct_pck_count > 0
            || direct_config_count > 0
            || has_resources
            || has_bin
            || has_lib
            || has_mods)
    {
        score += 30;
        strong_anchor = true;
        integrity_kind = "app_bundle".to_string();
        evidence.push("dll+config_bundle".to_string());
        app_signals.push(format!("dll:{direct_dll_count}"));
    } else if direct_dll_count > 0 {
        score += 4;
        evidence.push("dll_weak_signal".to_string());
        app_signals.push(format!("dll:{direct_dll_count}"));
    }
    if direct_font_count >= 2 && direct_file_count <= 6 && direct_dir_count == 0 {
        score += 45;
        strong_anchor = true;
        integrity_kind = "doc_bundle".to_string();
        evidence.push("font_pack".to_string());
        app_signals.push("font_pack".to_string());
    }
    if direct_text_count >= 6 && document_collection_share >= 75 {
        score += 30;
        strong_anchor = true;
        if integrity_kind == "mixed" {
            integrity_kind = "doc_bundle".to_string();
        }
        evidence.push("document_bundle".to_string());
        app_signals.push("document_bundle".to_string());
    } else if direct_archive_count > 0 && direct_text_count >= 3 {
        score += 18;
        if integrity_kind == "mixed" {
            integrity_kind = "doc_bundle".to_string();
        }
        evidence.push("archive+documents".to_string());
        app_signals.push("archive+documents".to_string());
    }
    if (direct_image_count + direct_video_count + direct_audio_count) >= 3
        && !paired_sidecars.is_empty()
    {
        score += 22;
        strong_anchor = true;
        if integrity_kind == "mixed" {
            integrity_kind = "media_bundle".to_string();
        }
        evidence.push("media_sidecars".to_string());
        app_signals.push("media_sidecars".to_string());
    }
    if metadata_marker_count > 0 && (direct_dir_count > 0 || direct_file_count >= 3) {
        score += 22;
        if integrity_kind == "mixed" {
            integrity_kind = "export_backup_bundle".to_string();
        }
        evidence.push("metadata_markers".to_string());
        app_signals.push("metadata_markers".to_string());
    }
    if direct_exe_count >= 1
        && package_doc_count > 0
        && direct_file_count <= 6
        && direct_dir_count == 0
        && direct_dll_count == 0
        && direct_archive_count == 0
    {
        score += 30;
        strong_anchor = true;
        if integrity_kind == "mixed" {
            integrity_kind = "app_bundle".to_string();
        }
        evidence.push("installer_with_docs".to_string());
        app_signals.push(format!("installer_docs:{package_doc_count}"));
    }
    if document_collection_layout {
        score += 14;
        strong_anchor = true;
        if integrity_kind == "mixed" {
            integrity_kind = "doc_bundle".to_string();
        }
        evidence.push("document_collection_layout".to_string());
        app_signals.push(format!("document_files:{}", direct_text_count));
    }
    if direct_cursor_count >= 8
        && (direct_image_count > 0 || direct_text_count > 0 || direct_inf_count > 0)
    {
        score += 34;
        strong_anchor = true;
        if integrity_kind == "mixed" {
            integrity_kind = "theme_pack".to_string();
        }
        evidence.push("cursor_theme_pack".to_string());
        app_signals.push(format!("cursor_files:{direct_cursor_count}"));
        if direct_inf_count > 0 {
            evidence.push("install_manifest_present".to_string());
        }
    }
    if multi_variant_app_bundle {
        score += 42;
        strong_anchor = true;
        if integrity_kind == "mixed" {
            integrity_kind = "app_bundle".to_string();
        }
        evidence.push("multi_variant_app_bundle".to_string());
        app_signals.push(format!("root_named_binaries:{root_named_binary_count}"));
        if package_doc_count > 0 {
            evidence.push("package_docs_present".to_string());
        }
    } else if package_doc_count > 0
        && root_named_file_count >= 1
        && (direct_exe_count > 0 || direct_dll_count > 0 || direct_archive_count > 0)
        && direct_file_count <= 12
    {
        score += 20;
        strong_anchor = true;
        if integrity_kind == "mixed" {
            integrity_kind = if direct_exe_count > 0 || direct_dll_count > 0 {
                "app_bundle".to_string()
            } else {
                "doc_bundle".to_string()
            };
        }
        evidence.push("package_docs_bundle".to_string());
        app_signals.push(format!("package_docs:{package_doc_count}"));
    }
    if max_family_count >= 2 {
        score += ((max_family_count as i32 - 1) * 4).clamp(4, 16);
        evidence.push(format!("nameFamilyCount={max_family_count}"));
    }
    if dominant_share >= 0.75 {
        score += 14;
        evidence.push("dominantExtensionHigh".to_string());
    } else if dominant_share >= 0.6 {
        score += 10;
        evidence.push("dominantExtension".to_string());
    }
    if direct_file_count >= 2
        && direct_file_count <= 5
        && ((direct_exe_count == 1 && (direct_bin_payload_count > 0 || direct_dll_count > 0))
            || (direct_dll_count > 0
                && (direct_json_count > 0 || direct_pck_count > 0 || direct_config_count > 0))
            || (direct_font_count >= 2 && direct_dir_count == 0))
    {
        score += 15;
        evidence.push("small_strong_bundle".to_string());
    }
    if prefer_whole && score > 0 {
        score += 6;
        evidence.push("collection_root_bonus".to_string());
    }

    let app_bundle_layout = strong_anchor
        && integrity_kind == "app_bundle"
        && direct_exe_count > 0
        && (direct_dll_count > 0
            || has_resources
            || has_bin
            || has_lib
            || direct_pak_count > 0
            || direct_bin_payload_count > 0);
    if app_bundle_layout {
        score += 8;
        evidence.push("app_layout_bundle".to_string());
    }

    if direct_dir_count >= 3
        && direct_file_count >= 6
        && direct_family_counts.len() >= 4
        && !strong_anchor
    {
        score -= 25;
        fragmentation_warnings.push("heterogeneous_top_level".to_string());
    }
    if file_count >= 6
        && dominant_share < 0.45
        && ext_counts.len() >= 5
        && !app_bundle_layout
        && !multi_variant_app_bundle
    {
        score -= 18;
        fragmentation_warnings.push("low_content_cohesion".to_string());
    }
    if max_family_count <= 1
        && direct_file_count >= 8
        && !app_bundle_layout
        && !document_collection_layout
    {
        score -= 12;
        fragmentation_warnings.push("weak_name_families".to_string());
    }
    if dll_only_directory {
        score -= 24;
        fragmentation_warnings.push("dll_only_directory".to_string());
    }

    if wrapper_target_path.is_some() {
        evidence.push("single_child_wrapper".to_string());
    }
    if staging_junk {
        fragmentation_warnings.push("runtime_cache_shell".to_string());
    }

    let integrity_score = score.clamp(0, 100) as u8;
    let explicit_split = (!strong_anchor
        && direct_dir_count >= 3
        && direct_file_count >= 6
        && direct_family_counts.len() >= 4)
        || (!strong_anchor
            && file_count >= 6
            && dominant_share < 0.45
            && ext_counts.len() >= 5
            && direct_family_counts.len() >= 4)
        || dll_only_directory
        || (integrity_kind == "mixed"
            && direct_dir_count >= 2
            && direct_file_count >= 6
            && direct_family_counts.len() >= 4);
    let result_kind = if wrapper_target_path.is_some() {
        DirectoryResultKind::WholeWrapperPassthrough
    } else if staging_junk {
        DirectoryResultKind::StagingJunk
    } else if explicit_split {
        DirectoryResultKind::MixedSplit
    } else {
        DirectoryResultKind::Whole
    };

    Some(DirectoryAssessment {
        result_kind,
        integrity_score,
        integrity_kind,
        evidence,
        wrapper_target_path,
        top_level_entries,
        dominant_extensions,
        name_families,
        paired_sidecars,
        fragmentation_warnings,
        naming_cohesion,
        total_size,
        file_count,
        dir_count,
        direct_file_count,
        direct_dir_count,
        max_depth,
    })
}

