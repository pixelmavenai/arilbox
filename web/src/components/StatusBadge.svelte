<script lang="ts">
  import { t } from '../lib/i18n';

  // Container state, job status or step status.
  let { status }: { status: string } = $props();

  const tone: Record<string, string> = {
    running: 'ok',
    succeeded: 'ok',
    starting: 'info',
    stopping: 'info',
    queued: 'info',
    pending: '',
    stopped: '',
    frozen: 'warn',
    compensated: 'warn',
    cancelled: 'warn',
    error: 'danger',
    failed: 'danger',
    compensation_failed: 'danger',
  };
</script>

<span class="badge {tone[status] ?? ''}">{t(`status.${status}`)}</span>
