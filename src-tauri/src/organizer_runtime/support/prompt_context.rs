fn summarize_directory_for_prompt(unit: &OrganizeUnit, response_language: &str) -> String {
    let Some(assessment) = unit.directory_assessment.as_ref() else {
        return if is_zh_language(response_language) {
            "暂无目录摘要。".to_string()
        } else {
            "No directory summary available.".to_string()
        };
    };
    let mut lines = vec![
        format!("resultKind={}", assessment.result_kind.as_str()),
        format!("integrityKind={}", assessment.integrity_kind),
        format!("integrityScore={}", assessment.integrity_score),
        format!("relativePath={}", unit.relative_path),
        format!("totalSize={}", unit.size),
        format!(
            "createdAt={}",
            unit.created_at
                .clone()
                .unwrap_or_else(|| organizer_unknown_label(response_language).to_string())
        ),
        format!(
            "modifiedAt={}",
            unit.modified_at
                .clone()
                .unwrap_or_else(|| organizer_unknown_label(response_language).to_string())
        ),
        format!(
            "directoryShape=directFiles:{}|directDirs:{}|totalFiles:{}|totalDirectories:{}|maxDepth:{}",
            assessment.direct_file_count,
            assessment.direct_dir_count,
            assessment.file_count,
            assessment.dir_count,
            assessment.max_depth
        ),
        format!(
            "evidence={}",
            if assessment.evidence.is_empty() {
                organizer_none_label(response_language).to_string()
            } else {
                assessment.evidence.join(", ")
            }
        ),
        format!("namingCohesion={}", assessment.naming_cohesion),
        format!(
            "topLevelEntries={}",
            if assessment.top_level_entries.is_empty() {
                organizer_none_label(response_language).to_string()
            } else {
                assessment.top_level_entries.join(", ")
            }
        ),
        format!(
            "dominantExtensions={}",
            if assessment.dominant_extensions.is_empty() {
                organizer_none_label(response_language).to_string()
            } else {
                assessment.dominant_extensions.join(", ")
            }
        ),
        format!(
            "nameFamilies={}",
            if assessment.name_families.is_empty() {
                organizer_none_label(response_language).to_string()
            } else {
                assessment.name_families.join(", ")
            }
        ),
        format!(
            "pairedSidecars={}",
            if assessment.paired_sidecars.is_empty() {
                organizer_none_label(response_language).to_string()
            } else {
                assessment.paired_sidecars.join(", ")
            }
        ),
        format!(
            "fragmentationWarnings={}",
            if assessment.fragmentation_warnings.is_empty() {
                organizer_none_label(response_language).to_string()
            } else {
                assessment.fragmentation_warnings.join(", ")
            }
        ),
    ];
    if is_zh_language(response_language) {
        lines.push(
            "该目录已是整体候选，默认按整体归类，除非摘要明确显示存在多个无关主题。".to_string(),
        );
    } else {
        lines.push(
            "This directory is already a bundle candidate. Default to classifying it as a whole unit unless the summary clearly indicates multiple unrelated themes."
                .to_string(),
        );
    }
    lines.join("\n")
}

#[allow(dead_code)]
fn build_reference_structure_context(
    root: &Path,
    excluded: &[String],
    stop: &AtomicBool,
    response_language: &str,
) -> String {
    let mut lines = Vec::new();
    let mut total_dirs = 0_u64;
    let mut total_files = 0_u64;
    let mut truncated = false;
    let max_lines = 240_usize;
    let max_depth = 10_usize;

    let walker = WalkDir::new(root)
        .min_depth(1)
        .max_depth(max_depth)
        .sort_by_file_name()
        .into_iter()
        .filter_entry(|entry| {
            if entry.depth() == 0 {
                return true;
            }
            let name = entry.file_name().to_string_lossy();
            !should_exclude(&name, excluded)
        });

    for entry in walker.filter_map(Result::ok) {
        if stop.load(Ordering::Relaxed) {
            truncated = true;
            break;
        }
        if lines.len() >= max_lines {
            truncated = true;
            break;
        }

        let relative = entry
            .path()
            .strip_prefix(root)
            .unwrap_or_else(|_| entry.path())
            .to_string_lossy()
            .replace('\\', "/");
        let depth = entry.depth().saturating_sub(1);
        let indent = "  ".repeat(depth);

        if entry.file_type().is_dir() {
            total_dirs = total_dirs.saturating_add(1);
            if is_zh_language(response_language) {
                lines.push(format!("{indent}[鐩綍] {relative}/"));
            } else {
                lines.push(format!("{indent}[D] {relative}/"));
            }
            continue;
        }
        if entry.file_type().is_file() {
            total_files = total_files.saturating_add(1);
            let size = entry.metadata().map(|meta| meta.len()).unwrap_or(0);
            if is_zh_language(response_language) {
                lines.push(format!("{indent}[鏂囦欢] {relative} ({size} bytes)"));
            } else {
                lines.push(format!("{indent}[F] {relative} ({size} bytes)"));
            }
        }
    }

    let mut out = if is_zh_language(response_language) {
        vec![
            format!("鏍硅矾寰?{}", root.to_string_lossy()),
            format!("参考树最大深度={max_depth}"),
            format!("参考树展示行数={}", lines.len()),
            format!("参考树目录数={total_dirs}"),
            format!("参考树文件数={total_files}"),
            format!("参考树是否截断={truncated}"),
            "参考树开始".to_string(),
        ]
    } else {
        vec![
            format!("rootPath={}", root.to_string_lossy()),
            format!("referenceTreeMaxDepth={max_depth}"),
            format!("referenceTreeLinesShown={}", lines.len()),
            format!("referenceTreeDirectoriesShown={total_dirs}"),
            format!("referenceTreeFilesShown={total_files}"),
            format!("referenceTreeTruncated={truncated}"),
            "referenceTreeStart".to_string(),
        ]
    };
    out.extend(lines);
    out.push(if is_zh_language(response_language) {
        "参考树结束".to_string()
    } else {
        "referenceTreeEnd".to_string()
    });
    out.join("\n")
}

