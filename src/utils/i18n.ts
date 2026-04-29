import { useEffect, useState } from 'react';
import type { Lang } from '../types';
import { translations, type TranslationKey } from './i18n-translations';

let currentLang: Lang = localStorage.getItem('appLang') === 'en' ? 'en' : 'zh';

export function getLang(): Lang {
  return currentLang;
}

export function setLang(lang: Lang): void {
  if (!translations[lang]) return;
  currentLang = lang;
  localStorage.setItem('appLang', lang);
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
