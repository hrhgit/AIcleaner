fn create_directory_unit(
    scan_root: &Path,
    path: &Path,
    metadata: &fs::Metadata,
    assessment: DirectoryAssessment,
) -> OrganizeUnit {
    OrganizeUnit {
        name: path
            .file_name()
            .and_then(|x| x.to_str())
            .unwrap_or_default()
            .to_string(),
        path: path.to_string_lossy().to_string(),
        relative_path: relative_path_string(scan_root, path),
        size: assessment.total_size,
        created_at: metadata.created().ok().map(system_time_to_iso),
        modified_at: metadata.modified().ok().map(system_time_to_iso),
        item_type: "directory".to_string(),
        modality: "directory".to_string(),
        directory_assessment: Some(assessment),
    }
}

fn create_file_unit(scan_root: &Path, path: &Path, metadata: &fs::Metadata) -> OrganizeUnit {
    OrganizeUnit {
        name: path
            .file_name()
            .and_then(|x| x.to_str())
            .unwrap_or_default()
            .to_string(),
        path: path.to_string_lossy().to_string(),
        relative_path: relative_path_string(scan_root, path),
        size: metadata.len(),
        created_at: metadata.created().ok().map(system_time_to_iso),
        modified_at: metadata.modified().ok().map(system_time_to_iso),
        item_type: "file".to_string(),
        modality: pick_modality(&path.to_string_lossy()).to_string(),
        directory_assessment: None,
    }
}

fn collect_directory_candidate(
    scan_root: &Path,
    path: &Path,
    recursive: bool,
    excluded: &[String],
    stop: &AtomicBool,
    parent_is_collection_root: bool,
    staging_passthrough_budget: u8,
    out: &mut Vec<OrganizeUnit>,
) {
    let metadata = match fs::metadata(path) {
        Ok(metadata) => metadata,
        Err(_) => return,
    };
    let assessment = match evaluate_directory_assessment(path, stop, parent_is_collection_root) {
        Some(assessment) => assessment,
        None => return,
    };

    match assessment.result_kind {
        DirectoryResultKind::Whole => {
            out.push(create_directory_unit(
                scan_root, path, &metadata, assessment,
            ));
        }
        DirectoryResultKind::WholeWrapperPassthrough => {
            if let Some(target) = assessment.wrapper_target_path.as_ref() {
                collect_directory_candidate(
                    scan_root,
                    Path::new(target),
                    recursive,
                    excluded,
                    stop,
                    parent_is_collection_root,
                    staging_passthrough_budget,
                    out,
                );
            }
        }
        DirectoryResultKind::MixedSplit => {
            if recursive {
                let current_is_collection_root = is_collection_root(path, excluded, stop);
                collect_units_inner(
                    scan_root,
                    path,
                    true,
                    excluded,
                    stop,
                    current_is_collection_root,
                    staging_passthrough_budget,
                    out,
                );
            }
        }
        DirectoryResultKind::StagingJunk => {
            if recursive && staging_passthrough_budget > 0 {
                collect_units_inner(
                    scan_root,
                    path,
                    true,
                    excluded,
                    stop,
                    false,
                    staging_passthrough_budget.saturating_sub(1),
                    out,
                );
            }
        }
    }
}

fn collect_units_inner(
    scan_root: &Path,
    current_dir: &Path,
    recursive: bool,
    excluded: &[String],
    stop: &AtomicBool,
    current_is_collection_root: bool,
    staging_passthrough_budget: u8,
    out: &mut Vec<OrganizeUnit>,
) {
    let entries = match fs::read_dir(current_dir) {
        Ok(entries) => entries,
        Err(_) => return,
    };

    for entry in entries.filter_map(Result::ok) {
        if stop.load(Ordering::Relaxed) {
            break;
        }
        let path = entry.path();
        let name = entry.file_name().to_string_lossy().to_string();
        if should_exclude(&name, excluded) {
            continue;
        }
        if path.is_dir() {
            collect_directory_candidate(
                scan_root,
                &path,
                recursive,
                excluded,
                stop,
                current_is_collection_root,
                staging_passthrough_budget,
                out,
            );
            continue;
        }
        if !path.is_file() {
            continue;
        }
        if let Ok(meta) = entry.metadata() {
            out.push(create_file_unit(scan_root, &path, &meta));
        }
    }
}

fn collect_units(
    root: &Path,
    recursive: bool,
    excluded: &[String],
    stop: &AtomicBool,
) -> Vec<OrganizeUnit> {
    let mut out = Vec::new();
    let root_is_collection_root = is_collection_root(root, excluded, stop);
    collect_units_inner(
        root,
        root,
        recursive,
        excluded,
        stop,
        root_is_collection_root,
        1,
        &mut out,
    );
    out.sort_by(|a, b| {
        a.relative_path
            .to_lowercase()
            .cmp(&b.relative_path.to_lowercase())
            .then_with(|| a.item_type.cmp(&b.item_type))
    });
    out
}

