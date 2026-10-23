// Light/dark theme: follows the system unless the user picks one.
export type Theme = 'system' | 'light' | 'dark';

const KEY = 'arilbox.theme';

function stored(): Theme {
  try {
    const v = localStorage.getItem(KEY);
    if (v === 'light' || v === 'dark') return v;
  } catch {
    // Storage may be blocked; fall back to the system theme.
  }
  return 'system';
}

class ThemeState {
  current: Theme = $state(stored());

  set(theme: Theme): void {
    this.current = theme;
    try {
      if (theme === 'system') localStorage.removeItem(KEY);
      else localStorage.setItem(KEY, theme);
    } catch {
      // Not persisted; still applied for this visit.
    }
    apply(theme);
  }
}

function apply(theme: Theme): void {
  if (theme === 'system') delete document.documentElement.dataset.theme;
  else document.documentElement.dataset.theme = theme;
}

export const theme = new ThemeState();
apply(theme.current);
