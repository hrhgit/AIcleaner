import type { PersistPolicy } from '../types';

export function readPersisted<T>(key: string, fallback: T): T {
  try {
    const raw = localStorage.getItem(key);
    if (raw === null) return fallback;
    return JSON.parse(raw) as T;
  } catch {
    return fallback;
  }
}

export function writePersisted<T>(key: string, value: T): void {
  try {
    localStorage.setItem(key, JSON.stringify(value));
  } catch {
    // localStorage quota or privacy failures should not block rendering.
  }
}

export function removePersisted(key: string): void {
  try {
    localStorage.removeItem(key);
  } catch {
    // ignore storage failures
  }
}

export function readPolicy<T>(policy: PersistPolicy<T>): T {
  if (policy.kind !== 'normal') return policy.defaultValue;
  try {
    const raw = localStorage.getItem(policy.key);
    if (raw === null) return policy.defaultValue;
    return policy.deserializer ? policy.deserializer(raw) : (JSON.parse(raw) as T);
  } catch {
    return policy.defaultValue;
  }
}

export function writePolicy<T>(policy: PersistPolicy<T>, value: T): void {
  if (policy.kind !== 'normal') return;
  try {
    localStorage.setItem(policy.key, policy.serializer ? policy.serializer(value) : JSON.stringify(value));
  } catch {
    // Persistence is a recovery aid, not a rendering dependency.
  }
}

export function formatSize(bytes: number | null | undefined): string {
  if (bytes == null || Number.isNaN(bytes)) return '0 B';
  if (bytes < 1024) return `${bytes} B`;
  if (bytes < 1024 * 1024) return `${(bytes / 1024).toFixed(1)} KB`;
  if (bytes < 1024 * 1024 * 1024) return `${(bytes / (1024 * 1024)).toFixed(1)} MB`;
  return `${(bytes / (1024 * 1024 * 1024)).toFixed(2)} GB`;
}

