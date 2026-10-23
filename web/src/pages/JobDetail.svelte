<script lang="ts">
  import JobSteps from '../components/JobSteps.svelte';
  import StatusBadge from '../components/StatusBadge.svelte';
  import { type Job, watchJob } from '../lib/api';
  import { formatDate, t } from '../lib/i18n';
  import { link } from '../lib/router.svelte';
  import { jobKind } from '../lib/util';

  // Live view of one job: the SSE stream sends a snapshot, then updates until it finishes.
  let { id }: { id: string } = $props();
  let job: Job | null = $state(null);
  let failed = $state(false);

  $effect(() => {
    job = null;
    failed = false;
    const timer = setTimeout(() => (failed = job === null), 5000);
    const stop = watchJob(id, (j) => (job = j));
    return () => {
      clearTimeout(timer);
      stop();
    };
  });
</script>

<div class="page-head">
  <h1>
    <a href="/jobs" use:link class="muted">{t('jobs.title')}</a> /
    {job ? `${jobKind(job.kind)}: ${job.target}` : id}
  </h1>
  {#if job}<StatusBadge status={job.status} />{/if}
</div>

{#if job}
  <div class="cards">
    <section class="card">
      <dl class="kv">
        <dt>{t('jobs.col.target')}</dt>
        <dd>
          {#if job.kind.startsWith('container_')}
            <a href="/containers/{job.target}" use:link>{job.target}</a>
          {:else}
            {job.target}
          {/if}
        </dd>
        <dt>{t('jobs.col.created')}</dt>
        <dd>{formatDate(job.created_at)}</dd>
        <dt>{t('job.started')}</dt>
        <dd>{formatDate(job.started_at)}</dd>
        <dt>{t('job.finished')}</dt>
        <dd>{formatDate(job.finished_at)}</dd>
        <dt>{t('job.id')}</dt>
        <dd class="mono">{job.id}</dd>
      </dl>
    </section>
  </div>
  <section class="card">
    <h2>{t('job.steps')}</h2>
    <JobSteps {job} />
  </section>
{:else if failed}
  <div class="card empty">{t('job.not_found')}</div>
{:else}
  <p class="empty">{t('common.loading')}</p>
{/if}
