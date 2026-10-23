<script lang="ts">
  import { activity } from '../lib/activity.svelte';
  import { t } from '../lib/i18n';
  import { link } from '../lib/router.svelte';
  import StatusBadge from './StatusBadge.svelte';
</script>

<div class="toasts" aria-live="polite">
  {#each activity.items as item (item.id)}
    {@const job = item.job}
    {@const current = job?.steps.find((s) => s.status === 'running')}
    <div class="toast card">
      <div class="head">
        <strong>{item.title}</strong>
        <StatusBadge status={job?.status ?? 'queued'} />
      </div>
      {#if current}
        <div class="muted">{t(`step.${current.id}`)}…</div>
      {/if}
      <div class="foot">
        <a href="/jobs/{item.id}" use:link>{t('job.details')}</a>
        {#if job?.finished_at}
          <button class="small" onclick={() => activity.dismiss(item.id)}>{t('action.dismiss')}</button>
        {/if}
      </div>
    </div>
  {/each}
</div>

<style>
  .toasts {
    position: fixed;
    inset-block-end: 1rem;
    inset-inline-end: 1rem;
    display: flex;
    flex-direction: column;
    gap: 0.5rem;
    inline-size: min(22rem, calc(100vw - 2rem));
    z-index: 10;
  }
  .toast {
    padding: 0.75rem 0.9rem;
  }
  .head,
  .foot {
    display: flex;
    justify-content: space-between;
    align-items: center;
    gap: 0.5rem;
  }
  .foot {
    margin-block-start: 0.4rem;
    font-size: 0.9rem;
  }
</style>
