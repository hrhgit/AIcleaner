import { useEffect, useState } from 'react';
import type { Lang } from '../types';
import { translations, type TranslationKey } from './i18n-translations';

const LANGUAGE_STORAGE_KEY = 'aicleaner.shell.global.language.v1';
const LEGACY_LANGUAGE_STORAGE_KEY = 'appLang';

function normalizeLang(value: string | null): Lang | null {
  return value === 'en' || value === 'zh' ? value : null;
}

function readStoredLang(): Lang {
  try {
    const scopedLang = normalizeLang(localStorage.getItem(LANGUAGE_STORAGE_KEY));
    if (scopedLang) return scopedLang;

    const legacyLang = normalizeLang(localStorage.getItem(LEGACY_LANGUAGE_STORAGE_KEY));
    if (legacyLang) {
      localStorage.setItem(LANGUAGE_STORAGE_KEY, legacyLang);
      localStorage.removeItem(LEGACY_LANGUAGE_STORAGE_KEY);
      return legacyLang;
    }
  } catch {
    // Language is a normal preference; rendering should continue if storage is unavailable.
  }
  return 'zh';
}

let currentLang: Lang = readStoredLang();

export function getLang(): Lang {
  return currentLang;
}

export function setLang(lang: Lang): void {
  if (!translations[lang]) return;
  currentLang = lang;
  try {
    localStorage.setItem(LANGUAGE_STORAGE_KEY, lang);
    localStorage.removeItem(LEGACY_LANGUAGE_STORAGE_KEY);
  } catch {
    // Keep the in-memory language active even if persistence fails.
  }
  document.documentElement.lang = currentLang;
  window.dispatchEvent(new Event('languageChanged'));
}

export function t(key: TranslationKey, params: Record<string, string> = {}): string {
  let str: string = translations[currentLang]?.[key] || translations.en[key] || key;
  for (const [k, v] of Object.entries(params)) {
    str = str.replace(`{${k}}`, v);
  }
  return str;
}

export function text(zh: string, en: string): string {
  return currentLang === 'en' ? en : zh;
}

export function registerLangChangeHandler(handler: () => void): () => void {
  window.addEventListener('languageChanged', handler);
  return () => window.removeEventListener('languageChanged', handler);
}

export function useLanguage(): [Lang, (lang: Lang) => void] {
  const [lang, setLocalLang] = useState<Lang>(currentLang);
  useEffect(() => registerLangChangeHandler(() => setLocalLang(currentLang)), []);
  return [lang, setLang];
}

document.documentElement.lang = currentLang;
