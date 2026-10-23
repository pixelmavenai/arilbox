<script lang="ts">
  import Console from '../components/Console.svelte';
  import ContainerActions from '../components/ContainerActions.svelte';
  import StatusBadge from '../components/StatusBadge.svelte';
  import { activity } from '../lib/activity.svelte';
  import { ApiError, type ContainerInfo, type Job, api } from '../lib/api';
  import { formatBytes, formatDate, formatPercent, t } from '../lib/i18n';
  import { CpuRate, poll } from '../lib/poll';
  import { link, navigate } from '../lib/router.svelte';
  import { errorText, jobKind } from '../lib/util';

  let { name }: { name: string } = $props();

  type Tab = 'overview' | 'console' | 'jobs';
  let tab: Tab = $state('overview');
  let container: ContainerInfo | null = $state(null);
  let missing = $state(false);
  let jobs: Job[] = $state([]);
  let cpu: number | null = $state(null);
  let error = $state('');
  const rate = new CpuRate();

  $effect(() => {
    void activity.version;
    const target = name;
    return poll(async () => {
      try {
        const c = await api.container(target);
        cpu = c.state === 'running' ? rate.sample(c.name, c.cpu_usage_ns) : null;
        container = c;
        missing = false;
        error = '';
      } catch (e) {
        if (e instanceof ApiError && e.status === 404) missing = true;
        else error = errorText(e);
      }
      // Recent jobs for this container (the API has no target filter yet).
      api.jobs(500).then((all) => (jobs = all.filter((j) => j.target === target).slice(0, 20))).catch(() => {});
    }, 5000);
  });
</script>

<div class="page-head">
  <h1>
    <a href="/containers" use:link class="muted">{t('containers.title')}</a> / {name}
    {#if container}<StatusBadge status={container.state} />{/if}
  </h1>
  {#if container}
    <ContainerActions {container} onerror={(m) => (error = m)} ondeleted={() => navigate('/containers')} />
  {/if}
</div>

{#if error}
  <div class="alert" role="alert">{error}</div>
{/if}

{#if missing}
  <div class="card empty">
    <p>{t('container.missing', { name })}</p>
    <a href="/containers" use:link>{t('container.back')}</a>
  </div>
{:else if container}
  <div class="tabs" role="tablist">
    {#each ['overview', 'console', 'jobs'] as const as id (id)}
      <button role="tab" aria-selected={tab === id} onclick={() => (tab = id)}>{t(`container.tab.${id}`)}</button>
    {/each}
  </div>

  {#if tab === 'overview'}
    <div class="cards">
      <section class="card">
        <h2>{t('container.overview')}</h2>
        <dl class="kv">
          <dt>{t('containers.col.state')}</dt>
          <dd><StatusBadge status={container.state} /></dd>
          <dt>{t('containers.col.template')}</dt>
          <dd>{t(`template.${container.template.distro}`)} {container.template.release}</dd>
          <dt>{t('container.created')}</dt>
          <dd>{formatDate(container.created_at)}</dd>
          <dt>{t('container.gpu')}</dt>
          <dd>{t(container.gpu ? 'common.yes' : 'common.no')}</dd>
        </dl>
      </section>
      <section class="card">
        <h2>{t('container.runtime')}</h2>
        <dl class="kv">
          <dt>{t('containers.col.ipv4')}</dt>
          <dd class="mono">{container.ipv4.join(', ') || '—'}</dd>
          <dt>{t('container.ipv6')}</dt>
          <dd class="mono">{container.ipv6.join(', ') || '—'}</dd>
          <dt>{t('container.pid')}</dt>
          <dd class="mono">{container.pid ?? '—'}</dd>
          <dt>{t('containers.col.cpu')}</dt>
          <dd>{cpu == null ? '—' : formatPercent(cpu / 100)}</dd>
          <dt>{t('containers.col.memory')}</dt>
          <dd>{container.state === 'running' ? formatBytes(container.memory_bytes) : '—'}</dd>
        </dl>
      </section>
    </div>
  {:else if tab === 'console'}
    {#if container.state === 'running'}
      {#key name}
        <Console {name} />
      {/key}
    {:else}
      <div class="card empty">{t('console.not_running')}</div>
    {/if}
  {:else}
    <div class="card">
      {#if jobs.length === 0}
        <p class="empty">{t('jobs.none')}</p>
      {:else}
        <div class="table-wrap">
          <table>
            <thead>
              <tr>
                <th>{t('jobs.col.kind')}</th>
                <th>{t('jobs.col.status')}</th>
                <th>{t('jobs.col.created')}</th>
              </tr>
            </thead>
            <tbody>
              {#each jobs as job (job.id)}
                <tr>
                  <td><a href="/jobs/{job.id}" use:link>{jobKind(job.kind)}</a></td>
                  <td><StatusBadge status={job.status} /></td>
                  <td>{formatDate(job.created_at)}</td>
                </tr>
              {/each}
            </tbody>
          </table>
        </div>
      {/if}
    </div>
  {/if}
{:else}
  <p class="empty">{t('common.loading')}</p>
{/if}

<style>
  h1 {
    display: flex;
    align-items: center;
    gap: 0.6rem;
    flex-wrap: wrap;
  }
</style>
