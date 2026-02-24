/**
 * src/utils/i18n.js
 * 国际化管理 — 支持中英双语切换
 */

const translations = {
    zh: {
        // Sidebar & Common
        'app.title': 'AIcleaner',
        'nav.settings': '设置',
        'nav.scanner': '扫描',
        'nav.results': '结果',
        'btn.language': 'EN / 中',
        'toast.success': '操作成功',
        'toast.error': '操作失败',
        'page.not_found': '页面未找到',

        // Settings Page
        'settings.title': '⚙️ 设置',
        'settings.subtitle': '配置 AI 分析引擎和扫描参数',
        'settings.api_config': '🔑 API 配置',
        'settings.llm_engine': 'LLM 引擎',
        'settings.provider': '服务商 (API Endpoint)',
        'settings.provider_hint': '选择提供大模型服务的厂商',
        'settings.api_key': 'API Key',
        'settings.api_key_placeholder': '在此处填写你的 API Key',
        'settings.api_key_hint': '密钥仅存储在本地服务器，不会上传到任何第三方',
        'settings.model': '模型设定',
        'settings.model_hint': '选择适用的模型（根据所选的服务商自动更新）',
        'settings.search_config': '🌐 联网与搜索设置',
        'settings.expert_feature': '专家功能',
        'settings.enable_search': '启用 AI 自动联网搜索 (通过 Tavily)',
        'settings.search_hint': '当大模型无法确定可疑文件的用途时，将自动调用搜索引擎进行辅助判断。建议在清理非系统盘、开发环境或未知用途的目录时开启此项以提高判定准确率。',
        'settings.tavily_key': 'Tavily API Key',
        'settings.tavily_hint': '前往 Tavily 官网申请免费 API Key (每月 1000 次查询)',
        'settings.scan_config': '📂 扫描配置',
        'settings.scan_params': '扫描参数',
        'settings.scan_path': '扫描路径',
        'settings.browse': '📁 浏览',
        'settings.browse_hint': '输入要扫描的文件夹绝对路径，或点击「浏览」选择',
        'settings.target_size': '期望清理空间',
        'settings.target_size_hint': '当可清理空间达到此目标时，扫描将自动停止',
        'settings.max_depth': '最大扫描深度',
        'settings.depth_unit': '层',
        'settings.max_depth_hint': '限制递归下探的目录层级数量',
        'settings.save': '💾 保存设置',
        'settings.saving': '保存中...',
        'settings.saved': '✓ 已保存',
        'settings.save_failed': '✗ 保存失败',
        'settings.toast_saved': '设置已保存',
        'settings.toast_save_failed': '保存失败: ',
        'settings.toast_path_selected': '已选择路径: ',
        'settings.toast_browse_failed': '选择文件夹失败: ',
        'settings.browsing': '⏳ 选择中...',

        // Scanner Page
        'scanner.title': '🔍 全局扫描',
        'scanner.subtitle': '分析磁盘空间，找出可安全清理的文件',
        'scanner.current_path': '📂 当前扫描目录',
        'scanner.not_set': '未配置',
        'scanner.start': '🚀 开始智能扫描',
        'scanner.pause': '⏸️ 暂停扫描',
        'scanner.resume': '▶️ 继续扫描',
        'scanner.stop': '⏹️ 停止扫描',
        'scanner.view_results': '📊 查看清理建议',
        'scanner.prepare': '📦 准备扫描...',
        'scanner.scanning': '正在扫描',
        'scanner.analyzing': '正在使用 AI 分析...',
        'scanner.completed': '✅ 扫描已完成！共发现 {count} 个可清理项。',
        'scanner.stopped': '扫描已终止',
        'scanner.path_not_configured': '尚未配置扫描目录。请前往设置页面进行配置。',
        'scanner.go_settings': '前往设置',
        'scanner.progress_scan': '扫描进度',
        'scanner.progress_ai': 'AI 分析进度',
        'scanner.activity_log': '📝 活动日志',
        'scanner.log_start': '---------- 扫描开始 ----------',
        'scanner.log_pause': '---------- 扫描已暂停 ----------',
        'scanner.log_resume': '---------- 扫描已恢复 ----------',
        'scanner.log_stop': '---------- 扫描已强行终止 ----------',
        'scanner.log_complete': '---------- 扫描完成 ----------',
        'scanner.toast_start_failed': '启动扫描失败: ',
        'scanner.toast_pause_failed': '暂停失败: ',
        'scanner.toast_resume_failed': '恢复失败: ',
        'scanner.toast_stop_failed': '停止失败: ',

        // Results Page
        'results.title': '📊 扫描结果',
        'results.subtitle': '查看 AI 的清理建议，并执行安全清理',
        'results.summary': '📈 清理概览',
        'results.safe_to_clean': '安全可清理',
        'results.space_freed': '预计可释放',
        'results.files_count': '文件数量',
        'results.items': '项',
        'results.scan_not_started': '尚无扫描数据，请先前往扫描页面开始扫描',
        'results.go_scan': '前往扫描',
        'results.clean_selected': '🧹 清理选中项',
        'results.filter_all': '全部',
        'results.filter_safe': '推荐清理 (Safe)',
        'results.filter_warning': '谨慎清理 (Warning)',
        'results.filter_danger': '不建议清理 (Danger)',
        'results.table_path': '路径 / 名称',
        'results.table_size': '大小',
        'results.table_reason': 'AI 判定理由',
        'results.table_action': '操作',
        'results.risk_safe': '安全',
        'results.risk_warning': '谨慎',
        'results.risk_danger': '高危',
        'results.risk_unknown': '未知',
        'results.open_folder': '📂 打开目录',
        'results.cleaning': '清理中...',
        'results.cleaned_success': '已成功清理选中的 {count} 个项目。',
        'results.toast_load_failed': '无法加载扫描结果: ',
        'results.toast_clean_failed': '清理操作失败: ',
        'results.toast_clean_completed': '清理完成',
        'results.toast_open_failed': '打开目录失败: ',
        'results.toast_no_selection': '请先勾选需要清理的项目。'
    },
    en: {
        // Sidebar & Common
        'app.title': 'AIcleaner',
        'nav.settings': 'Settings',
        'nav.scanner': 'Scan',
        'nav.results': 'Results',
        'btn.language': '中 / EN',
        'toast.success': 'Success',
        'toast.error': 'Error',
        'page.not_found': 'Page Not Found',

        // Settings Page
        'settings.title': '⚙️ Settings',
        'settings.subtitle': 'Configure AI Analysis Engine and Scan Parameters',
        'settings.api_config': '🔑 API Configuration',
        'settings.llm_engine': 'LLM Engine',
        'settings.provider': 'Provider (API Endpoint)',
        'settings.provider_hint': 'Select the LLM service provider',
        'settings.api_key': 'API Key',
        'settings.api_key_placeholder': 'Enter your API Key here',
        'settings.api_key_hint': 'Keys are only stored locally and not sent to any third party.',
        'settings.model': 'Model Settings',
        'settings.model_hint': 'Select the applicable model (updates automatically based on provider)',
        'settings.search_config': '🌐 Web Search Configuration',
        'settings.expert_feature': 'Expert Feature',
        'settings.enable_search': 'Enable AI Web Search (via Tavily)',
        'settings.search_hint': 'When the LLM cannot determine a file\'s purpose, it will automatically use search to assist. Recommended when cleaning non-system drives, dev environments, or unknown directories.',
        'settings.tavily_key': 'Tavily API Key',
        'settings.tavily_hint': 'Get a free API Key from Tavily (1000 free queries/month)',
        'settings.scan_config': '📂 Scan Configuration',
        'settings.scan_params': 'Scan Parameters',
        'settings.scan_path': 'Scan Path',
        'settings.browse': '📁 Browse',
        'settings.browse_hint': 'Enter the absolute path or click "Browse" to select',
        'settings.target_size': 'Target Clean Size',
        'settings.target_size_hint': 'The scan will stop automatically when this target is reached',
        'settings.max_depth': 'Max Scan Depth',
        'settings.depth_unit': 'lv',
        'settings.max_depth_hint': 'Limit the depth of directory recursion',
        'settings.save': '💾 Save Settings',
        'settings.saving': 'Saving...',
        'settings.saved': '✓ Saved',
        'settings.save_failed': '✗ Save Failed',
        'settings.toast_saved': 'Settings saved successfully',
        'settings.toast_save_failed': 'Save failed: ',
        'settings.toast_path_selected': 'Path selected: ',
        'settings.toast_browse_failed': 'Failed to browse folders: ',
        'settings.browsing': '⏳ Browsing...',

        // Scanner Page
        'scanner.title': '🔍 Global Scan',
        'scanner.subtitle': 'Analyze disk space and find safe-to-clean files',
        'scanner.current_path': '📂 Current Path',
        'scanner.not_set': 'Not Set',
        'scanner.start': '🚀 Start Smart Scan',
        'scanner.pause': '⏸️ Pause Scan',
        'scanner.resume': '▶️ Resume Scan',
        'scanner.stop': '⏹️ Stop Scan',
        'scanner.view_results': '📊 View Recommendations',
        'scanner.prepare': '📦 Preparing...',
        'scanner.scanning': 'Scanning',
        'scanner.analyzing': 'Analyzing with AI...',
        'scanner.completed': '✅ Scan complete! Found {count} items to clean.',
        'scanner.stopped': 'Scan stopped',
        'scanner.path_not_configured': 'Scan directory is not configured. Please go to settings.',
        'scanner.go_settings': 'Go to Settings',
        'scanner.progress_scan': 'Scan Progress',
        'scanner.progress_ai': 'AI Analysis Progress',
        'scanner.activity_log': '📝 Activity Log',
        'scanner.log_start': '---------- Scan Started ----------',
        'scanner.log_pause': '---------- Scan Paused ----------',
        'scanner.log_resume': '---------- Scan Resumed ----------',
        'scanner.log_stop': '---------- Scan Stopped ----------',
        'scanner.log_complete': '---------- Scan Completed ----------',
        'scanner.toast_start_failed': 'Failed to start scan: ',
        'scanner.toast_pause_failed': 'Failed to pause: ',
        'scanner.toast_resume_failed': 'Failed to resume: ',
        'scanner.toast_stop_failed': 'Failed to stop: ',

        // Results Page
        'results.title': '📊 Scan Results',
        'results.subtitle': 'Review AI recommendations and perform safe cleanup',
        'results.summary': '📈 Overview',
        'results.safe_to_clean': 'Safe to Clean',
        'results.space_freed': 'Est. Space Freed',
        'results.files_count': 'File Count',
        'results.items': 'items',
        'results.scan_not_started': 'No scan data available. Please run a scan first.',
        'results.go_scan': 'Go to Scan',
        'results.clean_selected': '🧹 Clean Selected',
        'results.filter_all': 'All',
        'results.filter_safe': 'Safe to Clean',
        'results.filter_warning': 'Warning',
        'results.filter_danger': 'Danger',
        'results.table_path': 'Path / Name',
        'results.table_size': 'Size',
        'results.table_reason': 'AI Reason',
        'results.table_action': 'Action',
        'results.risk_safe': 'Safe',
        'results.risk_warning': 'Warning',
        'results.risk_danger': 'Danger',
        'results.risk_unknown': 'Unknown',
        'results.open_folder': '📂 Open Folder',
        'results.cleaning': 'Cleaning...',
        'results.cleaned_success': 'Successfully cleaned {count} items.',
        'results.toast_load_failed': 'Failed to load results: ',
        'results.toast_clean_failed': 'Clean operation failed: ',
        'results.toast_clean_completed': 'Cleanup completed',
        'results.toast_open_failed': 'Failed to open folder: ',
        'results.toast_no_selection': 'Please select items to clean first.'
    }
};

let currentLang = localStorage.getItem('appLang') || 'zh';

export function getLang() {
    return currentLang;
}

export function setLang(lang) {
    if (translations[lang]) {
        currentLang = lang;
        localStorage.setItem('appLang', lang);
        applyTranslationsToDOM();
    }
}

export function toggleLang() {
    const newLang = currentLang === 'zh' ? 'en' : 'zh';
    setLang(newLang);
    return newLang;
}

export function t(key, params = {}) {
    let str = translations[currentLang][key] || key;
    for (const [k, v] of Object.entries(params)) {
        str = str.replace(`{${k}}`, v);
    }
    return str;
}

export function applyTranslationsToDOM() {
    const elements = document.querySelectorAll('[data-i18n]');
    elements.forEach(el => {
        const key = el.getAttribute('data-i18n');
        el.textContent = t(key);
    });

    // update document language
    document.documentElement.lang = currentLang;
}

// Initialize on load
setTimeout(applyTranslationsToDOM, 0);

// Custom event to notify pages of language changes
export function registerLangChangeHandler(handler) {
    window.addEventListener('languageChanged', handler);
}

export function emitLangChange() {
    window.dispatchEvent(new Event('languageChanged'));
}
