<script lang="ts">
  import { activity } from '../lib/activity.svelte';
  import { type Accepted, type ContainerInfo, api } from '../lib/api';
  import { t } from '../lib/i18n';
  import { errorText } from '../lib/util';
  import DeleteContainer from './DeleteContainer.svelte';

  // Start / stop / restart / delete for one container. Each starts a job shown as a toast.
  let {
    container,
    compact = false,
    onerror,
    ondeleted,
  }: {
    container: ContainerInfo;
    compact?: boolean;
    onerror: (message: string) => void;
    ondeleted?: () => void;
  } = $props();

  let busy = $state(false);
  let deleting = $state(false);

  const running = $derived(container.state === 'running');
  const stopped = $derived(container.state === 'stopped');
  const pending = $derived(
    activity.items.some((i) => i.job?.target === container.name && !i.job.finished_at),
  );

  async function run(action: 'start' | 'stop' | 'restart', call: () => Promise<Accepted>) {
    busy = true;
    try {
      const { job_id } = await call();
      void activity.track(job_id, t(`action.${action}.title`, { name: container.name }));
    } catch (e) {
      onerror(errorText(e));
    } finally {
      busy = false;
    }
  }
</script>

<div class="actions">
  <button
    class:small={compact}
    disabled={busy || pending || !stopped}
    onclick={() => run('start', () => api.start(container.name))}>{t('action.start')}</button
  >
  <button
    class:small={compact}
    disabled={busy || pending || !running}
    onclick={() => run('stop', () => api.stop(container.name))}>{t('action.stop')}</button
  >
  <button
    class:small={compact}
    disabled={busy || pending || !(running || stopped)}
    onclick={() => run('restart', () => api.restart(container.name))}>{t('action.restart')}</button
  >
  <button
    class="danger"
    class:small={compact}
    disabled={busy || pending}
    onclick={() => (deleting = true)}>{t('action.delete')}</button
  >
</div>

<DeleteContainer bind:open={deleting} {container} {onerror} {ondeleted} />
