use super::support::*;
use super::*;

#[test]
fn category_inventory_groups_history_without_summaries() {
    let mut tree = default_tree();
    let contract_node = ensure_path(&mut tree, &["文档".to_string(), "合同协议".to_string()]);
    let media_node = ensure_path(&mut tree, &["媒体".to_string(), "图片".to_string()]);

    let inventory = summary::build_category_inventory(
        &tree,
        &[
            json!({
                "leafNodeId": contract_node.clone(),
                "categoryPath": ["文档", "合同协议"],
                "name": "租赁合同.pdf",
                "relativePath": "contracts\\租赁合同.pdf",
                "summaryText": "should not be copied",
                "representation": { "long": "should not be copied" }
            }),
            json!({
                "leafNodeId": contract_node.clone(),
                "categoryPath": ["文档", "合同协议"],
                "name": "服务协议.docx",
                "relativePath": "contracts\\服务协议.docx"
            }),
            json!({
                "leafNodeId": contract_node.clone(),
                "categoryPath": ["文档", "合同协议"],
                "name": "采购协议.pdf",
                "relativePath": "2024\\采购协议.pdf"
            }),
            json!({
                "leafNodeId": contract_node.clone(),
                "categoryPath": ["文档", "合同协议"],
                "name": "补充协议.pdf",
                "relativePath": "2024\\补充协议.pdf"
            }),
            json!({
                "leafNodeId": media_node.clone(),
                "categoryPath": ["媒体", "图片"],
                "name": "cover.png",
                "relativePath": "images\\cover.png"
            }),
            json!({
                "leafNodeId": "",
                "category": CATEGORY_CLASSIFICATION_ERROR,
                "reason": RESULT_REASON_CLASSIFICATION_ERROR,
                "name": "bad.txt"
            }),
        ],
        3,
    );

    assert_eq!(inventory.len(), 2);
    let contract_entry = inventory
        .iter()
        .find(|entry| entry.get("nodeId").and_then(Value::as_str) == Some(&contract_node))
        .expect("contract inventory exists");
    assert_eq!(contract_entry.get("count").and_then(Value::as_u64), Some(4));
    assert_eq!(
        contract_entry
            .get("files")
            .and_then(Value::as_array)
            .map(Vec::len),
        Some(3)
    );
    assert_eq!(
        contract_entry.get("truncated").and_then(Value::as_bool),
        Some(true)
    );
    assert!(contract_entry.get("summaryText").is_none());
    assert!(contract_entry.get("representation").is_none());

    let media_entry = inventory
        .iter()
        .find(|entry| entry.get("nodeId").and_then(Value::as_str) == Some(&media_node))
        .expect("media inventory exists");
    assert_eq!(media_entry.get("count").and_then(Value::as_u64), Some(1));
    assert_eq!(
        media_entry.get("truncated").and_then(Value::as_bool),
        Some(false)
    );
}

#[test]
fn collection_root_detects_download_like_root() {
    let root = temp_dir("download-root").join("Download");
    fs::create_dir_all(root.join("Buzz-1.4.2-Windows-X64")).expect("create buzz dir");
    fs::create_dir_all(root.join("QuickRestart")).expect("create quick dir");
    fs::create_dir_all(root.join("Fonts")).expect("create fonts dir");
    fs::create_dir_all(root.join("Docs")).expect("create docs dir");
    write_file(&root.join("setup.exe"));
    write_file(&root.join("paper.pdf"));
    write_file(&root.join("archive.zip"));
    write_file(&root.join("image.png"));

    let stop = AtomicBool::new(false);
    let mut report = CollectionReport::default();
    assert!(is_collection_root(
        &root,
        &normalize_excluded(None),
        &stop,
        &mut report
    ));

    let _ = fs::remove_dir_all(root.parent().unwrap_or(&root));
}

#[test]
fn evaluates_whole_for_app_bundle_directory() {
    let root = temp_dir("app-bundle");
    fs::create_dir_all(&root).expect("create root");
    write_file(&root.join("Buzz-1.4.2-windows.exe"));
    write_file(&root.join("Buzz-1.4.2-windows-1.bin"));
    write_file(&root.join("Buzz-1.4.2-windows-2.bin"));

    let stop = AtomicBool::new(false);
    let assessment = assess_directory(&root, &stop, true);
    assert_eq!(assessment.result_kind, DirectoryResultKind::Whole);

    let _ = fs::remove_dir_all(&root);
}

#[test]
fn evaluates_whole_for_plugin_bundle_directory() {
    let root = temp_dir("plugin-bundle");
    fs::create_dir_all(&root).expect("create root");
    write_file(&root.join("QuickRestart.dll"));
    write_file(&root.join("QuickRestart.json"));
    write_file(&root.join("QuickRestart.pck"));

    let stop = AtomicBool::new(false);
    let assessment = assess_directory(&root, &stop, true);
    assert_eq!(assessment.result_kind, DirectoryResultKind::Whole);

    let _ = fs::remove_dir_all(&root);
}

#[test]
fn evaluates_whole_for_font_pack_directory() {
    let root = temp_dir("font-pack");
    fs::create_dir_all(&root).expect("create root");
    write_file(&root.join("generica.otf"));
    write_file(&root.join("generica bold.otf"));

    let stop = AtomicBool::new(false);
    let assessment = assess_directory(&root, &stop, false);
    assert_eq!(assessment.result_kind, DirectoryResultKind::Whole);

    let _ = fs::remove_dir_all(&root);
}

#[test]
fn evaluates_whole_for_document_bundle_directory() {
    let root = temp_dir("doc-bundle");
    fs::create_dir_all(root.join("我的女友是冒险游戏（待续）")).expect("create child dir");
    for idx in 0..8 {
        write_file(&root.join(format!("chapter-{idx}.txt")));
    }
    write_file(&root.join("collection.zip"));
    write_file(&root.join("extras.zip"));

    let stop = AtomicBool::new(false);
    let assessment = assess_directory(&root, &stop, true);
    assert_eq!(assessment.result_kind, DirectoryResultKind::Whole);

    let _ = fs::remove_dir_all(&root);
}

#[test]
fn evaluates_wrapper_passthrough_for_single_child_shell() {
    let root = temp_dir("wrapper");
    let shell = root.join("DwnlData");
    let target = shell.join("32858");
    fs::create_dir_all(&target).expect("create target");
    write_file(&target.join("app.exe"));
    write_file(&target.join("payload.bin"));

    let stop = AtomicBool::new(false);
    let assessment = assess_directory(&shell, &stop, true);
    assert_eq!(
        assessment.result_kind,
        DirectoryResultKind::WholeWrapperPassthrough
    );
    assert_eq!(
        assessment.wrapper_target_path.as_deref(),
        Some(target.to_string_lossy().as_ref())
    );

    let units = collect_units(&root, true, &normalize_excluded(None), &stop).units;
    assert!(units.iter().any(
        |unit| unit.item_type == "directory" && unit.relative_path.ends_with("DwnlData\\32858")
    ));
    assert!(!units
        .iter()
        .any(|unit| unit.item_type == "directory" && unit.relative_path == "DwnlData"));

    let _ = fs::remove_dir_all(&root);
}

#[test]
fn evaluates_mixed_split_for_mixed_directory() {
    let root = temp_dir("mixed");
    fs::create_dir_all(root.join("photos")).expect("create photos dir");
    fs::create_dir_all(root.join("docs")).expect("create docs dir");
    fs::create_dir_all(root.join("tools")).expect("create tools dir");
    write_file(&root.join("setup.exe"));
    write_file(&root.join("paper.pdf"));
    write_file(&root.join("cover.png"));
    write_file(&root.join("song.mp3"));
    write_file(&root.join("font.ttf"));
    write_file(&root.join("notes.txt"));

    let stop = AtomicBool::new(false);
    let assessment = assess_directory(&root, &stop, false);
    assert_eq!(assessment.result_kind, DirectoryResultKind::MixedSplit);

    let _ = fs::remove_dir_all(&root);
}

#[test]
fn evaluates_staging_junk_for_runtime_cache_shell() {
    let root = temp_dir("junk");
    fs::create_dir_all(root.join("logs")).expect("create logs dir");
    fs::create_dir_all(root.join("cache")).expect("create cache dir");
    write_file(&root.join("telemetry_cache.json"));
    write_file(&root.join("update_cache.json"));
    write_file(&root.join("session.dat"));

    let stop = AtomicBool::new(false);
    let assessment = assess_directory(&root, &stop, false);
    assert_eq!(assessment.result_kind, DirectoryResultKind::StagingJunk);

    let _ = fs::remove_dir_all(&root);
}

#[test]
fn evaluates_whole_for_complex_windows_app_directory() {
    let root = temp_dir("windows-app");
    fs::create_dir_all(root.join("Config")).expect("create config dir");
    fs::create_dir_all(root.join("Images")).expect("create images dir");
    write_file(&root.join("App.exe"));
    for idx in 0..10 {
        write_file(&root.join(format!("runtime-{idx}.dll")));
    }
    for idx in 0..6 {
        write_file(&root.join(format!("asset-{idx}.png")));
    }
    for idx in 0..6 {
        write_file(&root.join(format!("strings-{idx}.res")));
    }
    write_file(&root.join("App.exe.config"));
    write_file(&root.join("readme.txt"));

    let stop = AtomicBool::new(false);
    let assessment = assess_directory(&root, &stop, true);
    assert_eq!(assessment.result_kind, DirectoryResultKind::Whole);
    assert_eq!(assessment.integrity_kind, "app_bundle");

    let _ = fs::remove_dir_all(&root);
}

#[test]
fn evaluates_whole_for_multi_variant_package_with_docs() {
    let root = temp_dir("dism-bundle").join("Dism++10.1.1002.1B");
    fs::create_dir_all(root.join("Config")).expect("create config dir");
    write_file(&root.join("Dism++ARM64.exe"));
    write_file(&root.join("Dism++x64.exe"));
    write_file(&root.join("Dism++x86.exe"));
    write_file(&root.join("ReadMe for NCleaner.txt"));
    write_file(&root.join("ReadMe for Dism++x86.txt"));
    write_file(&root.join("Dism++x86 usage notes.txt"));

    let stop = AtomicBool::new(false);
    let assessment = assess_directory(&root, &stop, true);
    assert_eq!(assessment.result_kind, DirectoryResultKind::Whole);
    assert_eq!(assessment.integrity_kind, "app_bundle");

    let _ = fs::remove_dir_all(root.parent().unwrap_or(&root));
}

#[test]
fn evaluates_whole_for_cursor_theme_pack_directory() {
    let root = temp_dir("cursor-pack").join("Nagasaki Soyo-theme-pack");
    fs::create_dir_all(root.join("optional-replacements")).expect("create alt dir");
    for name in [
        "Alternate.ani",
        "Busy.ani",
        "Diagonal Resize 1.ani",
        "Diagonal Resize 2.ani",
        "Help Select.ani",
        "Horizontal Resize.ani",
        "Link.ani",
        "Location Select.ani",
        "Move.ani",
        "Normal Select.ani",
        "Person Select.ani",
        "Precision Select.ani",
        "Text Select.ani",
        "Unavailable.ani",
        "Vertical Resize.ani",
        "Work.ani",
        "Arrow.cur",
        "Hand.cur",
    ] {
        write_file(&root.join(name));
    }
    write_file(&root.join("cursor-preview.jpg"));
    write_file(&root.join("license.txt"));
    write_file(&root.join("install.inf"));

    let stop = AtomicBool::new(false);
    let assessment = assess_directory(&root, &stop, true);
    assert_eq!(assessment.result_kind, DirectoryResultKind::Whole);
    assert_eq!(assessment.integrity_kind, "theme_pack");

    let _ = fs::remove_dir_all(root.parent().unwrap_or(&root));
}

#[test]
fn evaluates_whole_for_document_collection_with_weak_name_families() {
    let root = temp_dir("doc-collection").join("MC-article-collection-1");
    fs::create_dir_all(root.join("我的女友是冒险游戏（待续）")).expect("create child dir");
    for name in [
        "article-01-prologue.txt",
        "article-02-background.txt",
        "article-03-character-notes.txt",
        "article-04-worldbuilding.txt",
        "article-05-chapter-outline.txt",
        "article-06-dialogue-draft.txt",
        "article-07-side-story.txt",
        "article-08-ending-notes.txt",
        "article-09-reading-guide.txt",
        "article-10-author-commentary.txt",
        "article-11-extra-scenes.txt",
        "article-12-appendix.txt",
    ] {
        write_file(&root.join(name));
    }
    for name in [
        "MC-article-collection-1.zip",
        "article-drafts-backup.zip",
        "reading-materials.zip",
        "game-notes-archive.zip",
        "extras.7z",
    ] {
        write_file(&root.join(name));
    }

    let stop = AtomicBool::new(false);
    let assessment = assess_directory(&root, &stop, true);
    assert_eq!(assessment.result_kind, DirectoryResultKind::Whole);
    assert_eq!(assessment.integrity_kind, "doc_bundle");

    let _ = fs::remove_dir_all(root.parent().unwrap_or(&root));
}

#[test]
fn evaluates_whole_for_single_installer_with_readme() {
    let root = temp_dir("installer-docs").join("IDM-main");
    fs::create_dir_all(&root).expect("create root");
    write_file(&root.join("IDM_v6.41.2_Setup_by-System3206.exe"));
    write_file(&root.join("README.md"));

    let stop = AtomicBool::new(false);
    let assessment = assess_directory(&root, &stop, true);
    assert_eq!(assessment.result_kind, DirectoryResultKind::Whole);
    assert_eq!(assessment.integrity_kind, "app_bundle");

    let _ = fs::remove_dir_all(root.parent().unwrap_or(&root));
}

#[test]
fn dll_only_directory_does_not_become_whole() {
    let root = temp_dir("dll-only");
    fs::create_dir_all(&root).expect("create root");
    write_file(&root.join("a.dll"));
    write_file(&root.join("b.dll"));
    write_file(&root.join("c.dll"));

    let stop = AtomicBool::new(false);
    let assessment = assess_directory(&root, &stop, true);
    assert_ne!(assessment.result_kind, DirectoryResultKind::Whole);

    let _ = fs::remove_dir_all(&root);
}
