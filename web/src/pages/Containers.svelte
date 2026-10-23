<script lang="ts">
  import ContainerActions from '../components/ContainerActions.svelte';
  import CreateContainer from '../components/CreateContainer.svelte';
  import StatusBadge from '../components/StatusBadge.svelte';
  import { activity } from '../lib/activity.svelte';
  import { type ContainerInfo, api } from '../lib/api';
  import { formatBytes, formatPercent, t } from '../lib/i18n';
  import { CpuRate, poll } from '../lib/poll';
  import { link } from '../lib/router.svelte';
  import { errorText } from '../lib/util';

  let containers: ContainerInfo[] | null = $state(null);
  let cpu: Record<string, number | null> = $state({});
  let error = $state('');
  let creating = $state(false);
  const rate = new CpuRate();

  async function load() {
    try {
      const list = await api.containers();
      const next: Record<string, number | null> = {};
      for (const c of list) {
        next[c.name] = c.state === 'running' ? rate.sample(c.name, c.cpu_usage_ns) : null;
      }
      containers = list;
      cpu = next;
      error = '';
    } catch (e) {
      error = errorText(e);
    }
  }

  $effect(() => {
    void activity.version;
    return poll(load, 5000);
  });
</script>

<div class="page-head">
  <h1>{t('containers.title')}</h1>
  <button class="primary" onclick={() => (creating = true)}>{t('containers.create')}</button>
</div>

{#if error}
  <div class="alert" role="alert">{error}</div>
{/if}

<div class="card">
  {#if containers === null}
    <p class="empty">{t('common.loading')}</p>
  {:else if containers.length === 0}
    <div class="empty">
      <p>{t('containers.empty')}</p>
      <button class="primary" onclick={() => (creating = true)}>{t('containers.create')}</button>
    </div>
  {:else}
    <div class="table-wrap">
      <table>
        <thead>
          <tr>
            <th>{t('containers.col.name')}</th>
            <th>{t('containers.col.state')}</th>
            <th>{t('containers.col.ipv4')}</th>
            <th>{t('containers.col.cpu')}</th>
            <th>{t('containers.col.memory')}</th>
            <th>{t('containers.col.template')}</th>
            <th><span class="sr-only">{t('containers.col.actions')}</span></th>
          </tr>
        </thead>
        <tbody>
          {#each containers as c (c.name)}
            <tr>
              <td><a href="/containers/{c.name}" use:link><strong>{c.name}</strong></a></td>
              <td><StatusBadge status={c.state} /></td>
              <td class="mono">{c.ipv4.join(', ') || '—'}</td>
              <td>{cpu[c.name] == null ? "—" : formatPercent((cpu[c.name] ?? 0) / 100)}</td>
              <td>{c.state === 'running' ? formatBytes(c.memory_bytes) : '—'}</td>
              <td>{t(`template.${c.template.distro}`)} {c.template.release}</td>
              <td><ContainerActions container={c} compact onerror={(m) => (error = m)} /></td>
            </tr>
          {/each}
        </tbody>
      </table>
    </div>
  {/if}
</div>

<CreateContainer bind:open={creating} />

<style>
  .sr-only {
    position: absolute;
    inline-size: 1px;
    block-size: 1px;
    overflow: hidden;
    clip-path: inset(50%);
    white-space: nowrap;
  }
</style>
