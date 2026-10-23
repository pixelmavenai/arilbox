<script lang="ts">
  import type { Snippet } from 'svelte';
  import { t } from '../lib/i18n';
  import { link, router } from '../lib/router.svelte';
  import { session } from '../lib/session.svelte';
  import Toasts from './Toasts.svelte';

  let { children }: { children: Snippet } = $props();
  let menuOpen = $state(false);

  const nav = [
    { href: '/', key: 'nav.dashboard', match: ['dashboard'] },
    { href: '/containers', key: 'nav.containers', match: ['containers', 'container'] },
    { href: '/jobs', key: 'nav.jobs', match: ['jobs', 'job'] },
    { href: '/settings', key: 'nav.settings', match: ['settings'] },
  ];
</script>

<div class="shell">
  <header>
    <a class="brand" href="/" use:link>
      <img src="/favicon.svg" alt="" width="24" height="24" />
      <span>{t('app.name')}</span>
    </a>
    <button
      class="menu"
      aria-expanded={menuOpen}
      aria-controls="nav"
      onclick={() => (menuOpen = !menuOpen)}>{t('nav.menu')}</button
    >
  </header>
  <nav id="nav" class:open={menuOpen} aria-label={t('nav.label')}>
    {#each nav as item (item.href)}
      <a
        href={item.href}
        use:link
        onclick={() => (menuOpen = false)}
        aria-current={item.match.includes(router.route.name) ? 'page' : undefined}>{t(item.key)}</a
      >
    {/each}
    <div class="user">
      <span class="muted">{session.user}</span>
      <button class="small" onclick={() => session.logout()}>{t('auth.logout')}</button>
    </div>
  </nav>
  <main>
    {@render children()}
  </main>
</div>
<Toasts />

<style>
  .shell {
    display: grid;
    grid-template-columns: 14rem 1fr;
    grid-template-rows: auto 1fr;
    min-block-size: 100vh;
  }
  header {
    grid-column: 1 / -1;
    display: flex;
    align-items: center;
    justify-content: space-between;
    padding-block: 0.6rem;
    padding-inline: 1rem;
    background: var(--surface);
    border-block-end: 1px solid var(--border);
  }
  .brand {
    display: flex;
    align-items: center;
    gap: 0.5rem;
    font-weight: 700;
    font-size: 1.1rem;
    color: var(--text);
  }
  .brand:hover {
    text-decoration: none;
  }
  .menu {
    display: none;
  }
  nav {
    display: flex;
    flex-direction: column;
    gap: 0.15rem;
    padding: 1rem 0.6rem;
    background: var(--surface);
    border-inline-end: 1px solid var(--border);
  }
  nav a {
    padding-block: 0.45rem;
    padding-inline: 0.75rem;
    border-radius: 6px;
    color: var(--text);
  }
  nav a:hover {
    background: var(--surface-2);
    text-decoration: none;
  }
  nav a[aria-current='page'] {
    background: var(--accent-soft);
    color: var(--accent);
    font-weight: 600;
  }
  .user {
    margin-block-start: auto;
    display: flex;
    align-items: center;
    justify-content: space-between;
    gap: 0.5rem;
    padding-block-start: 1rem;
    padding-inline: 0.75rem;
    border-block-start: 1px solid var(--border);
  }
  main {
    padding: 1.5rem;
    min-inline-size: 0;
    max-inline-size: 80rem;
    padding-block-end: 8rem;
  }
  @media (max-width: 760px) {
    .shell {
      grid-template-columns: 1fr;
      grid-template-rows: auto auto 1fr;
    }
    .menu {
      display: inline-flex;
    }
    nav {
      display: none;
      border-inline-end: none;
      border-block-end: 1px solid var(--border);
    }
    nav.open {
      display: flex;
    }
    .user {
      margin-block-start: 0.5rem;
    }
    main {
      padding: 1rem;
      padding-block-end: 8rem;
    }
  }
</style>
