export function handleElevationTransition({
    showToast,
    t,
    delayMs = 4000,
} = {}) {
    if (typeof showToast === 'function' && typeof t === 'function') {
        showToast(t('settings.elevation_reload_hint'), 'info');
    }

    if (typeof window === 'undefined') {
        return;
    }

    window.setTimeout(() => {
        try {
            window.location.reload();
        } catch {
            // Best-effort reload only.
        }
    }, delayMs);
}
