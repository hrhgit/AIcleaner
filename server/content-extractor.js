import { readFile } from 'fs/promises';
import { dirname, extname } from 'path';
import { pathToFileURL } from 'url';
import { createRequire } from 'module';

const TEXT_EXTENSIONS = new Set([
    '.txt', '.md', '.markdown', '.json', '.jsonl', '.csv', '.tsv', '.xml', '.yaml', '.yml',
    '.js', '.jsx', '.ts', '.tsx', '.mjs', '.cjs', '.py', '.java', '.go', '.rs', '.c', '.h', '.cpp', '.hpp',
    '.css', '.scss', '.less', '.html', '.htm', '.vue', '.svelte', '.ini', '.log', '.conf', '.env', '.sql'
]);

const IMAGE_EXTENSIONS = new Set(['.png', '.jpg', '.jpeg', '.webp', '.gif', '.bmp']);
const VIDEO_EXTENSIONS = new Set(['.mp4', '.mov', '.mkv', '.avi', '.wmv', '.flv', '.webm', '.m4v']);
const AUDIO_EXTENSIONS = new Set(['.mp3', '.wav', '.m4a', '.aac', '.flac', '.ogg', '.wma', '.opus']);
const OFFICE_EXTENSIONS = new Set(['.docx', '.xlsx', '.pptx', '.odt', '.ods', '.odp', '.pdf', '.rtf']);

let officeParserModulePromise = null;
let cachedPdfWorkerSrc;
let hasResolvedPdfWorkerSrc = false;

const require = createRequire(import.meta.url);
const BALANCED_TEXT_CHAR_LIMIT = 8000;
const DEEP_TEXT_CHAR_LIMIT = 48000;

function buildTextPayload(text, maxChars) {
    const raw = String(text || '');
    if (raw.length <= maxChars) {
        return {
            text: raw,
            truncated: false,
            originalLength: raw.length,
        };
    }

    return {
        text: raw.slice(0, maxChars),
        truncated: true,
        originalLength: raw.length,
    };
}

function getOfficeParserModule() {
    if (!officeParserModulePromise) {
        officeParserModulePromise = import('officeparser').then((mod) => mod.default || mod.OfficeParser || mod);
    }
    return officeParserModulePromise;
}

function resolvePdfWorkerSrc() {
    if (hasResolvedPdfWorkerSrc) {
        return cachedPdfWorkerSrc || '';
    }

    hasResolvedPdfWorkerSrc = true;
    const candidates = [
        'pdfjs-dist/legacy/build/pdf.worker.mjs',
        'pdfjs-dist/build/pdf.worker.mjs',
    ];

    const officeParserLookupPaths = [];
    try {
        const officeParserPkg = require.resolve('officeparser/package.json');
        officeParserLookupPaths.push(dirname(officeParserPkg));
    } catch {
        // Ignore and use default node resolution below.
    }

    for (const candidate of candidates) {
        try {
            cachedPdfWorkerSrc = pathToFileURL(require.resolve(candidate)).href;
            return cachedPdfWorkerSrc;
        } catch {
            // Try next strategy.
        }

        if (officeParserLookupPaths.length > 0) {
            try {
                cachedPdfWorkerSrc = pathToFileURL(
                    require.resolve(candidate, { paths: officeParserLookupPaths })
                ).href;
                return cachedPdfWorkerSrc;
            } catch {
                // Try next candidate.
            }
        }
    }

    cachedPdfWorkerSrc = '';
    return '';
}

function toDataUrl(buffer, ext) {
    const mimeMap = {
        '.png': 'image/png',
        '.jpg': 'image/jpeg',
        '.jpeg': 'image/jpeg',
        '.webp': 'image/webp',
        '.gif': 'image/gif',
        '.bmp': 'image/bmp',
    };
    const mime = mimeMap[ext] || 'application/octet-stream';
    return `data:${mime};base64,${buffer.toString('base64')}`;
}

export function fileTypeOf(filePath) {
    const ext = extname(filePath).toLowerCase();
    if (IMAGE_EXTENSIONS.has(ext)) return 'image';
    if (VIDEO_EXTENSIONS.has(ext)) return 'video';
    if (AUDIO_EXTENSIONS.has(ext)) return 'audio';
    if (OFFICE_EXTENSIONS.has(ext)) return 'office';
    if (TEXT_EXTENSIONS.has(ext)) return 'text';
    return 'binary';
}

export async function extractFileContent(filePath, mode, options = {}) {
    const type = fileTypeOf(filePath);
    const supportsMultimodal = !!options.supportsMultimodal;

    if (mode === 'fast') {
        return { type, degraded: false, payload: null, warnings: [] };
    }

    const warnings = [];

    if (type === 'text') {
        try {
            const text = await readFile(filePath, 'utf-8');
            const charLimit = mode === 'balanced' ? BALANCED_TEXT_CHAR_LIMIT : DEEP_TEXT_CHAR_LIMIT;
            const payload = buildTextPayload(text, charLimit);
            if (payload.truncated) {
                warnings.push(`content_truncated:${payload.originalLength}->${payload.text.length}`);
            }
            return { type, degraded: false, payload, warnings };
        } catch (err) {
            return {
                type,
                degraded: true,
                payload: null,
                warnings: [`text_read_failed:${err.message}`],
            };
        }
    }

    if (type === 'image') {
        if (mode !== 'deep') {
            return { type, degraded: false, payload: null, warnings };
        }

        if (!supportsMultimodal) {
            return {
                type,
                degraded: true,
                payload: null,
                warnings: ['multimodal_not_supported'],
            };
        }

        try {
            const buf = await readFile(filePath);
            const ext = extname(filePath).toLowerCase();
            return {
                type,
                degraded: false,
                payload: {
                    imageDataUrl: toDataUrl(buf, ext),
                },
                warnings,
            };
        } catch (err) {
            return {
                type,
                degraded: true,
                payload: null,
                warnings: [`image_read_failed:${err.message}`],
            };
        }
    }

    if (type === 'office') {
        try {
            const officeParser = await getOfficeParserModule();
            const parserOptions = {};
            if (extname(filePath).toLowerCase() === '.pdf') {
                const pdfWorkerSrc = resolvePdfWorkerSrc();
                if (pdfWorkerSrc) {
                    parserOptions.pdfWorkerSrc = pdfWorkerSrc;
                }
            }
            const ast = await officeParser.parseOffice(filePath, parserOptions);
            const text = typeof ast?.toText === 'function' ? ast.toText() : '';
            if (!text) {
                warnings.push('office_text_empty');
            }

            const charLimit = mode === 'balanced' ? BALANCED_TEXT_CHAR_LIMIT : DEEP_TEXT_CHAR_LIMIT;
            const payload = buildTextPayload(text, charLimit);
            if (payload.truncated) {
                warnings.push(`content_truncated:${payload.originalLength}->${payload.text.length}`);
            }
            return { type, degraded: false, payload, warnings };
        } catch (err) {
            return {
                type,
                degraded: true,
                payload: null,
                warnings: [`office_parse_failed:${err.message}`],
            };
        }
    }

    if (mode === 'balanced') {
        return {
            type,
            degraded: true,
            payload: null,
            warnings: ['balanced_mode_non_text_no_payload'],
        };
    }

    return {
        type,
        degraded: true,
        payload: null,
        warnings: ['deep_mode_unsupported_file_type'],
    };
}
