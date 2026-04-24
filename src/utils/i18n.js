/**
 * src/utils/i18n.js
 * Internationalization utilities
 */

import { translations } from './i18n-translations.js';
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
  let str = translations[currentLang]?.[key] || translations.en?.[key] || key;
  for (const [k, v] of Object.entries(params)) {
    str = str.replace(`{${k}}`, v);
  }
  return str;
}

export function applyTranslationsToDOM() {
  const elements = document.querySelectorAll('[data-i18n]');
  elements.forEach((el) => {
    const key = el.getAttribute('data-i18n');
    el.textContent = t(key);
  });

  document.documentElement.lang = currentLang;
}

setTimeout(applyTranslationsToDOM, 0);

export function registerLangChangeHandler(handler) {
  window.addEventListener('languageChanged', handler);
  return () => window.removeEventListener('languageChanged', handler);
}

export function emitLangChange() {
  window.dispatchEvent(new Event('languageChanged'));
}
