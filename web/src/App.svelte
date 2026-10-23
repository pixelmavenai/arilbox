<script lang="ts">
  import Shell from './components/Shell.svelte';
  import { t } from './lib/i18n';
  import { navigate, router } from './lib/router.svelte';
  import { session } from './lib/session.svelte';
  import ContainerDetail from './pages/ContainerDetail.svelte';
  import Containers from './pages/Containers.svelte';
  import Dashboard from './pages/Dashboard.svelte';
  import JobDetail from './pages/JobDetail.svelte';
  import Jobs from './pages/Jobs.svelte';
  import Login from './pages/Login.svelte';
  import NotFound from './pages/NotFound.svelte';
  import Settings from './pages/Settings.svelte';
  import Setup from './pages/Setup.svelte';

  void session.check();

  // Send people to setup, login or the app depending on the session.
  $effect(() => {
    if (!session.checked) return;
    const name = router.route.name;
    if (session.setupRequired) {
      if (name !== 'setup') navigate('/setup', true);
    } else if (!session.user) {
      if (name !== 'login') {
        const here = router.path === '/' ? '' : `?next=${encodeURIComponent(router.path + router.search)}`;
        navigate(`/login${here}`, true);
      }
    } else if (name === 'login' || name === 'setup') {
      navigate('/', true);
    }
  });

  const titles: Record<string, string> = {
    setup: 'setup.title',
    login: 'auth.login.title',
    dashboard: 'dashboard.title',
    containers: 'containers.title',
    jobs: 'jobs.title',
    settings: 'settings.title',
  };

  $effect(() => {
    const route = router.route;
    const page =
      route.name === 'container'
        ? route.params.name
        : route.name === 'job'
          ? t('job.title')
          : t(titles[route.name] ?? 'not_found.title');
    document.title = `${page} · ${t('app.name')}`;
  });
</script>

{#if !session.checked}
  <p class="empty">{t('common.loading')}</p>
{:else if router.route.name === 'setup' && session.setupRequired}
  <Setup />
{:else if !session.user}
  {#if router.route.name === 'login'}
    <Login />
  {/if}
{:else}
  <Shell>
    {#if router.route.name === 'dashboard'}
      <Dashboard />
    {:else if router.route.name === 'containers'}
      <Containers />
    {:else if router.route.name === 'container'}
      <ContainerDetail name={router.route.params.name} />
    {:else if router.route.name === 'jobs'}
      <Jobs />
    {:else if router.route.name === 'job'}
      <JobDetail id={router.route.params.id} />
    {:else if router.route.name === 'settings'}
      <Settings />
    {:else}
      <NotFound />
    {/if}
  </Shell>
{/if}
