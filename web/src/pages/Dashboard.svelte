<script lang="ts">
  import Meter from '../components/Meter.svelte';
  import StatusBadge from '../components/StatusBadge.svelte';
  import { activity } from '../lib/activity.svelte';
  import { type Capabilities, type ContainerInfo, type HostInfo, type HostStats, type Job, api } from '../lib/api';
  import {
    formatBytes,
    formatDate,
    formatDuration,
    formatNumber,
    formatPercent,
    t,
  } from '../lib/i18n';
  import { poll } from '../lib/poll';
  import { link } from '../lib/router.svelte';
  import { errorText, jobKind } from '../lib/util';

  let host: HostInfo | null = $state(null);
  let stats: HostStats | null = $state(null);
  let caps: Capabilities | null = $state(null);
  let containers: ContainerInfo[] = $state([]);
  let jobs: Job[] = $state([]);
  let error = $state('');

  const running = $derived(containers.filter((c) => c.state === 'running'));

  $effect(() => {
    api.host().then((h) => (host = h)).catch((e) => (error = errorText(e)));
    api.capabilities().then((c) => (caps = c)).catch(() => {});
  });

  $effect(() => {
    void activity.version; // Refresh when a job from this browser finishes.
    return poll(async () => {
      try {
        [stats, containers, jobs] = await Promise.all([api.hostStats(), api.containers(), api.jobs(8)]);
        error = '';
      } catch (e) {
        error = errorText(e);
      }
    }, 5000);
  });
</script>

<div class="page-head"><h1>{t('dashboard.title')}</h1></div>

{#if error}
  <div class="alert" role="alert">{error}</div>
{/if}

<div class="cards">
  <section class="card">
    <h2>{t('dashboard.host')}</h2>
    {#if host}
      <dl class="kv">
        <dt>{t('host.hostname')}</dt>
        <dd>{host.hostname}</dd>
        <dt>{t('host.os')}</dt>
        <dd>{host.os}</dd>
        <dt>{t('host.kernel')}</dt>
        <dd class="mono">{host.kernel}</dd>
        <dt>{t('host.cpus')}</dt>
        <dd>{host.cpu_count}</dd>
        <dt>{t('host.uptime')}</dt>
        <dd>{formatDuration(stats?.uptime_s ?? host.uptime_s)}</dd>
        <dt>{t('host.version')}</dt>
        <dd>{host.platform_version}</dd>
      </dl>
    {/if}
  </section>

  <section class="card">
    <h2>{t('dashboard.usage')}</h2>
    {#if stats}
      <Meter label={t('host.cpu')} value={stats.cpu_percent / 100} detail={formatPercent(stats.cpu_percent / 100)} />
      <Meter
        label={t('host.memory')}
        value={stats.memory_used_bytes / stats.memory_total_bytes}
        detail={t('dashboard.used_of', {
          used: formatBytes(stats.memory_used_bytes),
          total: formatBytes(stats.memory_total_bytes),
        })}
      />
      {#if stats.swap_total_bytes > 0}
        <Meter
          label={t('host.swap')}
          value={stats.swap_used_bytes / stats.swap_total_bytes}
          detail={t('dashboard.used_of', {
            used: formatBytes(stats.swap_used_bytes),
            total: formatBytes(stats.swap_total_bytes),
          })}
        />
      {/if}
      <div class="muted">
        {t('host.load')}: {stats.load.map((l) => formatNumber(l)).join(' · ')}
      </div>
    {/if}
  </section>

  <section class="card">
    <h2>{t('dashboard.features')}</h2>
    {#if caps}
      <ul class="features">
        <li>
          <span>{t('feature.zfs')}</span>
          <span class="badge {caps.zfs ? 'ok' : ''}">{t(caps.zfs ? 'feature.available' : 'feature.missing')}</span>
        </li>
        <li>
          <span>{t('feature.gpu')}</span>
          <span class="badge {caps.nvidia ? 'ok' : ''}">{t(caps.nvidia ? 'feature.available' : 'feature.no_gpu')}</span>
        </li>
        <li>
          <span>{t('feature.cgroup_v2')}</span>
          <span class="badge {caps.cgroup_v2 ? 'ok' : 'danger'}">{t(caps.cgroup_v2 ? 'feature.available' : 'feature.missing')}</span>
        </li>
        <li>
          <span>{t('feature.kvm')}</span>
          <span class="badge {caps.kvm ? 'ok' : ''}">{t(caps.kvm ? 'feature.available' : 'feature.missing')}</span>
        </li>
      </ul>
    {/if}
  </section>
</div>

<div class="cards">
  <section class="card">
    <div class="page-head">
      <h2>{t('dashboard.running', { running: running.length, total: containers.length })}</h2>
      <a href="/containers" use:link>{t('dashboard.all_containers')}</a>
    </div>
    {#if running.length === 0}
      <p class="empty">{t('dashboard.none_running')}</p>
    {:else}
      <ul class="list">
        {#each running as c (c.name)}
          <li>
            <a href="/containers/{c.name}" use:link>{c.name}</a>
            <span class="mono muted">{c.ipv4[0] ?? '—'}</span>
            <span class="muted">{formatBytes(c.memory_bytes)}</span>
          </li>
        {/each}
      </ul>
    {/if}
  </section>

  <section class="card">
    <div class="page-head">
      <h2>{t('dashboard.recent_jobs')}</h2>
      <a href="/jobs" use:link>{t('dashboard.all_jobs')}</a>
    </div>
    {#if jobs.length === 0}
      <p class="empty">{t('jobs.none')}</p>
    {:else}
      <ul class="list">
        {#each jobs as job (job.id)}
          <li>
            <a href="/jobs/{job.id}" use:link>{jobKind(job.kind)}: {job.target}</a>
            <span class="muted">{formatDate(job.created_at)}</span>
            <StatusBadge status={job.status} />
          </li>
        {/each}
      </ul>
    {/if}
  </section>
</div>

<style>
  .features,
  .list {
    list-style: none;
    margin: 0;
    padding: 0;
  }
  .features li,
  .list li {
    display: flex;
    justify-content: space-between;
    align-items: center;
    gap: 0.75rem;
    padding-block: 0.4rem;
    border-block-end: 1px solid var(--border);
    flex-wrap: wrap;
  }
  .features li:last-child,
  .list li:last-child {
    border-block-end: none;
  }
  .page-head h2 {
    margin: 0;
  }
</style>
