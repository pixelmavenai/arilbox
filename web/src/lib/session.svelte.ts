// Who is logged in, and whether first-run setup is pending.
import { api, setUnauthenticatedHandler } from './api';
import { navigate, router } from './router.svelte';

class Session {
  user: string | null = $state(null);
  setupRequired = $state(false);
  checked = $state(false);

  async check(): Promise<void> {
    try {
      this.setupRequired = (await api.setupStatus()).required;
      if (!this.setupRequired) this.user = (await api.me()).username;
    } catch {
      this.user = null;
    } finally {
      this.checked = true;
    }
  }

  loggedIn(username: string): void {
    this.user = username;
    this.setupRequired = false;
    const next = router.route.query.get('next');
    // Only same-site paths, never `//host` or a scheme.
    navigate(next && next.startsWith('/') && !next.startsWith('//') ? next : '/', true);
  }

  async logout(): Promise<void> {
    try {
      await api.logout();
    } finally {
      this.user = null;
      navigate('/login', true);
    }
  }
}

export const session = new Session();

setUnauthenticatedHandler(() => {
  if (session.user === null) return;
  session.user = null;
  const here = router.path + router.search;
  navigate(`/login?next=${encodeURIComponent(here)}`, true);
});
