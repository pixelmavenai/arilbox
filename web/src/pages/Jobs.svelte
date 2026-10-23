<script lang="ts">
  import StatusBadge from '../components/StatusBadge.svelte';
  import { activity } from '../lib/activity.svelte';
  import { type Job, api } from '../lib/api';
  import { formatDate, formatDuration, t } from '../lib/i18n';
  import { poll } from '../lib/poll';
  import { link } from '../lib/router.svelte';
  import { errorText, jobKind } from '../lib/util';

  let jobs: Job[] | null = $state(null);
  let error = $state('');

  function duration(job: Job): string {
    if (!job.started_at) return '—';
    const end = job.finished_at ? Date.parse(job.finished_at) : Date.now();
    return formatDuration((end - Date.parse(job.started_at)) / 1000);
  }

  $effect(() => {
    void activity.version;
    return poll(async () => {
      try {
        jobs = await api.jobs(100);
        error = '';
      } catch (e) {
        error = errorText(e);
      }
    }, 3000);
  });
</script>

<div class="page-head"><h1>{t('jobs.title')}</h1></div>

{#if error}
  <div class="alert" role="alert">{error}</div>
{/if}

<div class="card">
  {#if jobs === null}
    <p class="empty">{t('common.loading')}</p>
  {:else if jobs.length === 0}
    <p class="empty">{t('jobs.none')}</p>
  {:else}
    <div class="table-wrap">
      <table>
        <thead>
          <tr>
            <th>{t('jobs.col.kind')}</th>
            <th>{t('jobs.col.target')}</th>
            <th>{t('jobs.col.status')}</th>
            <th>{t('jobs.col.created')}</th>
            <th>{t('jobs.col.duration')}</th>
          </tr>
        </thead>
        <tbody>
          {#each jobs as job (job.id)}
            <tr>
              <td><a href="/jobs/{job.id}" use:link>{jobKind(job.kind)}</a></td>
              <td>{job.target}</td>
              <td><StatusBadge status={job.status} /></td>
              <td>{formatDate(job.created_at)}</td>
              <td>{duration(job)}</td>
            </tr>
          {/each}
        </tbody>
      </table>
    </div>
  {/if}
</div>
