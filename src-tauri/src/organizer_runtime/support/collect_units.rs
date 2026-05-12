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

fn compute_is_collection_root_from_scan(
    dir_name: &str,
    direct_dir_count: u64,
    direct_file_count: u64,
    file_families: &HashSet<String>,
) -> bool {
    let lower = dir_name.to_ascii_lowercase();
    let has_download_name = DOWNLOAD_ROOT_TOKENS
        .iter()
        .any(|token| lower.contains(token));
    (has_download_name && direct_dir_count >= 4)
        || (direct_dir_count >= 8 && direct_file_count >= 6 && file_families.len() >= 4)
        || (direct_dir_count >= 12 && file_families.len() >= 3)
}

fn aggregate_subtree_stats(
    direct_total_size: u64,
    profile: &DirectoryProfile,
    children: &[SubtreeStats],
) -> SubtreeStats {
    let direct_file_count = profile.direct_file_names.len() as u64;
    let direct_dir_count = profile.direct_dir_names.len() as u64;

    let mut total = SubtreeStats {
        total_size: direct_total_size,
        file_count: direct_file_count,
        dir_count: direct_dir_count,
        ext_counts: HashMap::new(),
        max_depth: 0,
    };

    // Collect direct extension counts from file names
    for name in &profile.direct_file_names {
        let key = extension_key(Path::new(name));
        *total.ext_counts.entry(key).or_insert(0) += 1u64;
    }

    for child in children {
        total.total_size = total.total_size.saturating_add(child.total_size);
        total.file_count = total.file_count.saturating_add(child.file_count);
        total.dir_count = total.dir_count.saturating_add(child.dir_count);
        for (ext, count) in &child.ext_counts {
            *total.ext_counts.entry(ext.clone()).or_insert(0) += count;
        }
        if !children.is_empty() {
            total.max_depth = total.max_depth.max(child.max_depth.saturating_add(1));
        }
        // Count the child directory itself
        total.dir_count = total.dir_count.saturating_add(1);
    }

    total
}

/// Quick wrapper check from direct scan data (no subtree needed).
fn provisional_wrapper_target(profile: &DirectoryProfile) -> Option<String> {
    let direct_file_count = profile.direct_file_names.len() as u64;
    let direct_dir_count = profile.direct_dir_names.len() as u64;
    if direct_dir_count == 1
        && ((direct_file_count == 0)
            || (direct_file_count <= 2
                && profile.direct_file_names.iter().all(|name| {
                    WRAPPER_FILE_EXTS
                        .iter()
                        .any(|ext| name.to_ascii_lowercase().ends_with(ext))
                })))
        && profile.marker_files.is_empty()
        && profile.direct_exe_count == 0
        && profile.direct_dll_count == 0
        && profile.direct_archive_count == 0
        && profile.direct_font_count == 0
        && profile.direct_script_count == 0
    {
        profile.direct_child_dirs.first().map(|c| c.to_string_lossy().to_string())
    } else {
        None
    }
}

/// Quick staging-junk check from direct scan data (no subtree needed).
fn provisional_staging_junk(profile: &DirectoryProfile) -> bool {
    let direct_file_count = profile.direct_file_names.len() as u64;
    let direct_dir_count = profile.direct_dir_names.len() as u64;
    let runtime_like_only = direct_file_count > 0
        && (profile.direct_runtime_count + profile.direct_config_count) as u64 * 100
            / direct_file_count
            >= 70
        && profile.direct_exe_count == 0
        && profile.direct_dll_count == 0
        && profile.direct_text_count <= 1
        && profile.direct_image_count == 0
        && profile.direct_video_count == 0
        && profile.direct_audio_count == 0
        && profile.direct_font_count == 0
        && profile.direct_script_count <= 1
        && profile.marker_files.is_empty();
    if runtime_like_only {
        profile.junk_named_dirs == direct_dir_count || direct_dir_count == 0
    } else {
        false
    }
}

/// Unified per‑directory handler: scan once, recurse bottom‑up, then decide Whole / MixedSplit.
/// Returns the subtree stats for aggregation by the parent.
fn collect_directory(
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
) -> SubtreeStats {
    if stop.load(Ordering::Relaxed) {
        report.stopped = true;
        return SubtreeStats::empty();
    }
    if depth > COLLECT_UNITS_MAX_DEPTH {
        report.depth_cap_hits = report.depth_cap_hits.saturating_add(1);
        return SubtreeStats::empty();
    }

    // ── Single scan ──
    let scan = match scan_directory_entries(path, excluded, stop) {
        Some(s) => s,
        None => {
            report.read_dir_errors = report.read_dir_errors.saturating_add(1);
            return SubtreeStats::empty();
        }
    };

    // ── Wrapper short‑circuit ──
    if let Some(target) = provisional_wrapper_target(&scan.profile) {
        // Don't create a unit for the wrapper shell; redirect to the inner directory.
        return collect_directory(
            collection_root,
            Path::new(&target),
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

    // ── Staging‑junk short‑circuit ──
    if provisional_staging_junk(&scan.profile) {
        if recursive && staging_passthrough_budget > 0 {
            // Treat as MixedSplit: push direct files, recurse with reduced budget.
            let mut child_out = Vec::new();
            let mut child_stats = Vec::new();
            for subdir_path in &scan.subdir_paths {
                let stats = collect_directory(
                    collection_root,
                    subdir_path,
                    recursive,
                    excluded,
                    stop,
                    false,
                    staging_passthrough_budget.saturating_sub(1),
                    depth.saturating_add(1),
                    &mut child_out,
                    report,
                );
                child_stats.push(stats);
            }
            for (file_path, meta) in &scan.file_entries {
                out.push(create_file_unit(collection_root, file_path, meta));
            }
            out.append(&mut child_out);
            return aggregate_subtree_stats(scan.direct_total_size, &scan.profile, &child_stats);
        }
        // Budget exhausted: treat as leaf, no further recursion.
        return SubtreeStats::empty();
    }

    // ── Normal path: recurse into children first (bottom‑up) ──
    let mut child_out = Vec::new();
    let mut child_stats = Vec::new();
    if recursive {
        for subdir_path in &scan.subdir_paths {
            let stats = collect_directory(
                collection_root,
                subdir_path,
                true,
                excluded,
                stop,
                parent_is_collection_root,
                staging_passthrough_budget,
                depth.saturating_add(1),
                &mut child_out,
                report,
            );
            child_stats.push(stats);
        }
    }

    let subtree =
        aggregate_subtree_stats(scan.direct_total_size, &scan.profile, &child_stats);

    let dir_name = path
        .file_name()
        .and_then(|x| x.to_str())
        .unwrap_or("");
    let assessment =
        compute_assessment(&scan.profile, &subtree, dir_name, parent_is_collection_root);

    let dir_metadata = match fs::metadata(path) {
        Ok(m) => m,
        Err(_) => {
            report.metadata_errors = report.metadata_errors.saturating_add(1);
            return subtree;
        }
    };

    match assessment.result_kind {
        DirectoryResultKind::Whole => {
            out.push(create_directory_unit(
                collection_root, path, &dir_metadata, assessment,
            ));
            // child_out is intentionally dropped — children are represented by the directory unit.
        }
        DirectoryResultKind::WholeWrapperPassthrough => {
            // Already handled by the early short‑circuit above; fall through to Whole.
            out.push(create_directory_unit(
                collection_root, path, &dir_metadata, assessment,
            ));
        }
        DirectoryResultKind::MixedSplit => {
            for (file_path, meta) in &scan.file_entries {
                if report.visited_files >= COLLECT_UNITS_MAX_FILES {
                    report.file_cap_hits = report.file_cap_hits.saturating_add(1);
                    break;
                }
                report.visited_files = report.visited_files.saturating_add(1);
                out.push(create_file_unit(collection_root, file_path, meta));
            }
            out.append(&mut child_out);
        }
        DirectoryResultKind::StagingJunk => {
            // Already handled by the early short‑circuit above; fall through to MixedSplit.
            for (file_path, meta) in &scan.file_entries {
                if report.visited_files >= COLLECT_UNITS_MAX_FILES {
                    report.file_cap_hits = report.file_cap_hits.saturating_add(1);
                    break;
                }
                report.visited_files = report.visited_files.saturating_add(1);
                out.push(create_file_unit(collection_root, file_path, meta));
            }
            out.append(&mut child_out);
        }
    }

    subtree
}

fn collect_units_inner(
    collection_root: &Path,
    current_dir: &Path,
    recursive: bool,
    excluded: &[String],
    stop: &AtomicBool,
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

    // ── Lightweight scan: collect files + track collection-root signals (no assessment classification) ──
    let mut file_entries: Vec<(PathBuf, fs::Metadata)> = Vec::new();
    let mut subdir_paths: Vec<PathBuf> = Vec::new();
    let mut direct_dir_count = 0u64;
    let mut direct_file_count = 0u64;
    let mut file_families = HashSet::<String>::new();

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
        let file_type = match entry.file_type() {
            Ok(ft) => ft,
            Err(_) => {
                report.non_file_entries = report.non_file_entries.saturating_add(1);
                continue;
            }
        };
        if file_type.is_dir() {
            direct_dir_count = direct_dir_count.saturating_add(1);
            subdir_paths.push(path);
            continue;
        }
        if file_type.is_symlink() || !file_type.is_file() {
            report.non_file_entries = report.non_file_entries.saturating_add(1);
            continue;
        }
        direct_file_count = direct_file_count.saturating_add(1);
        file_families.insert(classify_extension_family(&extension_key(&path)).to_string());

        if let Ok(meta) = entry.metadata() {
            file_entries.push((path, meta));
        } else {
            report.metadata_errors = report.metadata_errors.saturating_add(1);
        }
    }

    // ── Compute collection-root status from scan data ──
    let current_is_collection_root = {
        let dir_name = current_dir
            .file_name()
            .and_then(|x| x.to_str())
            .unwrap_or("");
        compute_is_collection_root_from_scan(
            dir_name,
            direct_dir_count,
            direct_file_count,
            &file_families,
        )
    };

    // ── Push collected file units ──
    for (file_path, meta) in file_entries {
        if report.visited_files >= COLLECT_UNITS_MAX_FILES {
            report.file_cap_hits = report.file_cap_hits.saturating_add(1);
            break;
        }
        report.visited_files = report.visited_files.saturating_add(1);
        out.push(create_file_unit(collection_root, &file_path, &meta));
    }

    // ── Process subdirectories (full scan + assessment via collect_directory) ──
    for subdir_path in subdir_paths {
        if report.visited_dirs >= COLLECT_UNITS_MAX_DIRS {
            report.dir_cap_hits = report.dir_cap_hits.saturating_add(1);
            break;
        }
        report.visited_dirs = report.visited_dirs.saturating_add(1);
        collect_directory(
            collection_root,
            &subdir_path,
            recursive,
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

fn collect_units(
    root: &Path,
    recursive: bool,
    excluded: &[String],
    stop: &AtomicBool,
) -> CollectionOutput {
    let mut out = Vec::new();
    let mut report = CollectionReport::default();
    collect_units_inner(
        root, root, recursive, excluded, stop, 0, 1, &mut out, &mut report,
    );
    out.sort_by(|a, b| {
        a.relative_path
            .to_lowercase()
            .cmp(&b.relative_path.to_lowercase())
            .then_with(|| a.item_type.cmp(&b.item_type))
    });
    CollectionOutput { units: out, report }
}
