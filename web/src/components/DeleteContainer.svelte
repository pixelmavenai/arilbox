<script lang="ts">
  import { activity } from '../lib/activity.svelte';
  import { type ContainerInfo, api } from '../lib/api';
  import { t } from '../lib/i18n';
  import { errorText } from '../lib/util';
  import Dialog from './Dialog.svelte';

  // Deleting needs the name typed back; a running container needs "force".
  let {
    open = $bindable(false),
    container,
    onerror,
    ondeleted,
  }: {
    open?: boolean;
    container: ContainerInfo;
    onerror: (message: string) => void;
    ondeleted?: () => void;
  } = $props();

  let typed = $state('');
  let force = $state(false);
  let busy = $state(false);

  const running = $derived(container.state !== 'stopped');
  const ready = $derived(typed === container.name && (!running || force) && !busy);

  $effect(() => {
    if (open) {
      typed = '';
      force = false;
    }
  });

  async function remove(e: SubmitEvent) {
    e.preventDefault();
    if (!ready) return;
    busy = true;
    try {
      const { job_id } = await api.deleteContainer(container.name, force);
      open = false;
      const job = activity.track(job_id, t('action.delete.title', { name: container.name }));
      void job.then((j) => {
        if (j.status === 'succeeded') ondeleted?.();
      });
    } catch (err) {
      onerror(errorText(err));
      open = false;
    } finally {
      busy = false;
    }
  }
</script>

<Dialog bind:open title={t('delete.title', { name: container.name })}>
  <form id="delete-form" onsubmit={remove}>
    <p>{t('delete.warning')}</p>
    {#if running}
      <label class="check">
        <input type="checkbox" bind:checked={force} />
        {t('delete.force')}
      </label>
    {/if}
    <div class="field">
      <label for="delete-confirm">{t('delete.confirm', { name: container.name })}</label>
      <input id="delete-confirm" type="text" autocomplete="off" spellcheck="false" bind:value={typed} />
    </div>
  </form>
  {#snippet footer()}
    <button type="button" onclick={() => (open = false)}>{t('action.cancel')}</button>
    <button type="submit" form="delete-form" class="danger solid" disabled={!ready}>
      {t('action.delete')}
    </button>
  {/snippet}
</Dialog>
