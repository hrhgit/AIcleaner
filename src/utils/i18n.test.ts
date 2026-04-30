import { afterEach, describe, expect, it, vi } from 'vitest';

const LANGUAGE_STORAGE_KEY = 'aicleaner.shell.global.language.v1';
const LEGACY_LANGUAGE_STORAGE_KEY = 'appLang';

async function loadI18n() {
  vi.resetModules();
  return import('./i18n');
}

afterEach(() => {
  localStorage.clear();
  document.documentElement.removeAttribute('lang');
  vi.restoreAllMocks();
});

describe('i18n language persistence', () => {
  it('restores the scoped language preference', async () => {
    localStorage.setItem(LANGUAGE_STORAGE_KEY, 'en');

    const { getLang } = await loadI18n();

    expect(getLang()).toBe('en');
    expect(document.documentElement.lang).toBe('en');
  });

  it('migrates the legacy language key', async () => {
    localStorage.setItem(LEGACY_LANGUAGE_STORAGE_KEY, 'en');

    const { getLang } = await loadI18n();

    expect(getLang()).toBe('en');
    expect(localStorage.getItem(LANGUAGE_STORAGE_KEY)).toBe('en');
    expect(localStorage.getItem(LEGACY_LANGUAGE_STORAGE_KEY)).toBeNull();
  });

  it('falls back to Chinese when storage has an invalid value', async () => {
    localStorage.setItem(LANGUAGE_STORAGE_KEY, 'de');

    const { getLang } = await loadI18n();

    expect(getLang()).toBe('zh');
  });

  it('keeps the in-memory language active if persistence fails', async () => {
    const { getLang, setLang } = await loadI18n();
    vi.spyOn(Storage.prototype, 'setItem').mockImplementation(() => {
      throw new Error('storage unavailable');
    });

    setLang('en');

    expect(getLang()).toBe('en');
    expect(document.documentElement.lang).toBe('en');
  });
});
