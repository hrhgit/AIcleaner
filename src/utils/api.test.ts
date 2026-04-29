import { describe, expect, it, vi } from 'vitest';

vi.mock('@tauri-apps/api/core', () => ({
  invoke: vi.fn(),
}));

vi.mock('@tauri-apps/api/event', () => ({
  listen: vi.fn(),
}));

describe('typed Tauri api', () => {
  it('preserves runtime boundary errors when Tauri internals are missing', async () => {
    const api = await import('./api');

    await expect(api.getSettings({ force: true })).rejects.toThrow('Unsupported runtime for command: settings_get');
  });
});

