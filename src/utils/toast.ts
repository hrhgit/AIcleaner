import type { ToastType } from '../types';

export function showToast(message: string, type: ToastType = 'info'): void {
  const toast = document.createElement('div');
  toast.className = `toast toast-${type}`;
  toast.textContent = message;
  document.body.appendChild(toast);
  window.setTimeout(() => {
    toast.classList.add('hide');
    window.setTimeout(() => toast.remove(), 220);
  }, 4200);
}

