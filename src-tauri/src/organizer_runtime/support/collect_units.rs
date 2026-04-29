const COLLECT_UNITS_MAX_FILES: u64 = 10_000;
const COLLECT_UNITS_MAX_DIRS: u64 = 3_000;
const COLLECT_UNITS_MAX_DEPTH: u32 = 10;

#[derive(Clone, Debug, Default, Serialize)]
struct CollectionReport {
    excluded_entries: u64,
    metadata_errors: u64,
    read_dir_errors: u64,
    assessment_skipped: u64,
    non_file_entries: u64,
    visited_files: u64,
    visited_dirs: u64,
    file_cap_hits: u64,
    dir_cap_hits: u64,
    depth_cap_hits: u64,
    assessment_file_cap_hits: u64,
    assessment_dir_cap_hits: u64,
    assessment_depth_cap_hits: u64,
    stopped: bool,
}

#[derive(Clone, Default)]
struct CollectionOutput {
    units: Vec<OrganizeUnit>,
    report: CollectionReport,
}

fn create_directory_unit(
    collection_root: &Path,
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
        relative_path: relative_path_string(collection_root, path),
        size: assessment.total_size,
        created_at: metadata.created().ok().map(system_time_to_iso),
        modified_at: metadata.modified().ok().map(system_time_to_iso),
        item_type: "directory".to_string(),
        modality: "directory".to_string(),
        directory_assessment: Some(assessment),
    }
}

fn create_file_unit(collection_root: &Path, path: &Path, metadata: &fs::Metadata) -> OrganizeUnit {
    OrganizeUnit {
        name: path
            .file_name()
            .and_then(|x| x.to_str())
            .unwrap_or_default()
            .to_string(),
        path: path.to_string_lossy().to_string(),
        relative_path: relative_path_string(collection_root, path),
        size: metadata.len(),
        created_at: metadata.created().ok().map(system_time_to_iso),
        modified_at: metadata.modified().ok().map(system_time_to_iso),
        item_type: "file".to_string(),
        modality: pick_modality(&path.to_string_lossy()).to_string(),
        directory_assessment: None,
    }
}

fn collect_directory_candidate(
    collection_root: &Path,
    path: &Path,
    recursive: bool,
    excluded: &[String],
    stop: &AtomicBool,
    parent_is_collection_root: bool,
    staging_passthrough_budget: u8,
    depth: u32,
    out: &mut Vec<OrganizeUnit>,
    report: &mut CollectionReport,
) {
    if stop.load(Ordering::Relaxed) {
        report.stopped = true;
        return;
    }
    if depth > COLLECT_UNITS_MAX_DEPTH {
        report.depth_cap_hits = report.depth_cap_hits.saturating_add(1);
        return;
    }
    let metadata = match fs::metadata(path) {
        Ok(metadata) => metadata,
        Err(_) => {
            report.metadata_errors = report.metadata_errors.saturating_add(1);
            return;
        }
    };
    let assessment = match evaluate_directory_assessment(
        path,
        excluded,
        stop,
        parent_is_collection_root,
        report,
    ) {
        Some(assessment) => assessment,
        None => {
            report.assessment_skipped = report.assessment_skipped.saturating_add(1);
            return;
        }
    };

    match assessment.result_kind {
        DirectoryResultKind::Whole => {
            out.push(create_directory_unit(
                collection_root, path, &metadata, assessment,
            ));
        }
        DirectoryResultKind::WholeWrapperPassthrough => {
            if let Some(target) = assessment.wrapper_target_path.as_ref() {
                collect_directory_candidate(
                    collection_root,
                    Path::new(target),
                    recursive,
                    excluded,
                    stop,
                    parent_is_collection_root,
                    staging_passthrough_budget,
                    depth.saturating_add(1),
                    out,
                    report,
                );
            }
        }
        DirectoryResultKind::MixedSplit => {
            if recursive {
                let current_is_collection_root = is_collection_root(path, excluded, stop, report);
                collect_units_inner(
                    collection_root,
                    path,
                    true,
                    excluded,
                    stop,
                    current_is_collection_root,
                    staging_passthrough_budget,
                    depth.saturating_add(1),
                    out,
                    report,
                );
            }
        }
        DirectoryResultKind::StagingJunk => {
            if recursive && staging_passthrough_budget > 0 {
                collect_units_inner(
                    collection_root,
                    path,
                    true,
                    excluded,
                    stop,
                    false,
                    staging_passthrough_budget.saturating_sub(1),
                    depth.saturating_add(1),
                    out,
                    report,
                );
            }
        }
    }
}

fn collect_units_inner(
    collection_root: &Path,
    current_dir: &Path,
    recursive: bool,
    excluded: &[String],
    stop: &AtomicBool,
    current_is_collection_root: bool,
    staging_passthrough_budget: u8,
    depth: u32,
    out: &mut Vec<OrganizeUnit>,
    report: &mut CollectionReport,
) {
    if depth > COLLECT_UNITS_MAX_DEPTH {
        report.depth_cap_hits = report.depth_cap_hits.saturating_add(1);
        return;
    }
    let entries = match fs::read_dir(current_dir) {
        Ok(entries) => entries,
        Err(_) => {
            report.read_dir_errors = report.read_dir_errors.saturating_add(1);
            return;
        }
    };

    for entry in entries.filter_map(Result::ok) {
        if stop.load(Ordering::Relaxed) {
            report.stopped = true;
            break;
        }
        let path = entry.path();
        let name = entry.file_name().to_string_lossy().to_string();
        if should_exclude(&name, excluded) {
            report.excluded_entries = report.excluded_entries.saturating_add(1);
            continue;
        }
        if path.is_dir() {
            if report.visited_dirs >= COLLECT_UNITS_MAX_DIRS {
                report.dir_cap_hits = report.dir_cap_hits.saturating_add(1);
                continue;
            }
            report.visited_dirs = report.visited_dirs.saturating_add(1);
            collect_directory_candidate(
                collection_root,
                &path,
                recursive,
                excluded,
                stop,
                current_is_collection_root,
                staging_passthrough_budget,
                depth.saturating_add(1),
                out,
                report,
            );
            continue;
        }
        if !path.is_file() {
            report.non_file_entries = report.non_file_entries.saturating_add(1);
            continue;
        }
        if report.visited_files >= COLLECT_UNITS_MAX_FILES {
            report.file_cap_hits = report.file_cap_hits.saturating_add(1);
            continue;
        }
        report.visited_files = report.visited_files.saturating_add(1);
        if let Ok(meta) = entry.metadata() {
            out.push(create_file_unit(collection_root, &path, &meta));
        } else {
            report.metadata_errors = report.metadata_errors.saturating_add(1);
        }
    }
}

fn collect_units(
    root: &Path,
    recursive: bool,
    excluded: &[String],
    stop: &AtomicBool,
) -> CollectionOutput {
    let mut out = Vec::new();
    let mut report = CollectionReport::default();
    let root_is_collection_root = is_collection_root(root, excluded, stop, &mut report);
    collect_units_inner(
        root,
        root,
        recursive,
        excluded,
        stop,
        root_is_collection_root,
        1,
        0,
        &mut out,
        &mut report,
    );
    out.sort_by(|a, b| {
        a.relative_path
            .to_lowercase()
            .cmp(&b.relative_path.to_lowercase())
            .then_with(|| a.item_type.cmp(&b.item_type))
    });
    CollectionOutput { units: out, report }
}

