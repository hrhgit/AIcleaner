import { beforeEach, describe, expect, it } from 'vitest';
import { readPersisted, readPolicy, writePersisted, writePolicy } from './storage';

describe('storage helpers', () => {
  beforeEach(() => {
    localStorage.clear();
  });

  it('falls back safely when persisted JSON is invalid', () => {
    localStorage.setItem('bad', '{');

    expect(readPersisted('bad', 'fallback')).toBe('fallback');
  });

  it('round trips normal persisted fields', () => {
    writePersisted('key', { ok: true });

    expect(readPersisted('key', { ok: false })).toEqual({ ok: true });
  });

  it('does not write sensitive policy fields to localStorage', () => {
    writePolicy({ key: 'secret', kind: 'sensitive', defaultValue: '' }, 'plain-secret');

    expect(localStorage.getItem('secret')).toBeNull();
    expect(readPolicy({ key: 'secret', kind: 'sensitive', defaultValue: 'fallback' })).toBe('fallback');
  });
});

