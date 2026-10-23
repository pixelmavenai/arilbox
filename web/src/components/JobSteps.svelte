<script lang="ts">
  import type { Job } from '../lib/api';
  import { formatDate, t, tOr } from '../lib/i18n';
  import StatusBadge from './StatusBadge.svelte';

  let { job, showLogs = true }: { job: Job; showLogs?: boolean } = $props();
</script>

<ol class="steps">
  {#each job.steps as step (step.id)}
    <li class={step.status}>
      <div class="head">
        <span>{tOr(`step.${step.id}`, step.label)}</span>
        <StatusBadge status={step.status} />
      </div>
      {#if showLogs && step.log.length > 0}
        <ul class="log mono">
          {#each step.log as line, i (i)}
            <li><time class="muted" datetime={line.at}>{formatDate(line.at)}</time> {line.message}</li>
          {/each}
        </ul>
      {/if}
    </li>
  {/each}
</ol>
{#if job.error}
  <div class="alert" role="alert">
    {tOr(`error.${job.error.code}`, job.error.message)}
    {#if job.error.rollback_incomplete}
      <div>{t('job.rollback_incomplete')}</div>
    {/if}
  </div>
{/if}

<style>
  .steps {
    list-style: none;
    margin: 0 0 1rem;
    padding: 0;
  }
  .steps > li {
    padding-block: 0.5rem;
    border-block-end: 1px solid var(--border);
  }
  .steps > li:last-child {
    border-block-end: none;
  }
  .head {
    display: flex;
    justify-content: space-between;
    align-items: center;
    gap: 1rem;
  }
  .pending .head span:first-child {
    color: var(--muted);
  }
  .log {
    list-style: none;
    margin: 0.4rem 0 0;
    padding: 0.5rem;
    background: var(--surface-2);
    border-radius: 6px;
    font-size: 0.8rem;
    max-block-size: 12rem;
    overflow: auto;
  }
</style>
