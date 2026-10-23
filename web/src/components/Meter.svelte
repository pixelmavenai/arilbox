<script lang="ts">
  // A labelled bar for a 0–1 ratio.
  let { label, value, detail }: { label: string; value: number; detail: string } = $props();

  const clamped = $derived(Math.min(1, Math.max(0, Number.isFinite(value) ? value : 0)));
  const tone = $derived(clamped > 0.9 ? 'danger' : clamped > 0.75 ? 'warn' : 'ok');
</script>

<div class="meter">
  <div class="row">
    <span>{label}</span>
    <span class="muted">{detail}</span>
  </div>
  <div
    class="track"
    role="meter"
    aria-label={label}
    aria-valuemin="0"
    aria-valuemax="100"
    aria-valuenow={Math.round(clamped * 100)}
  >
    <div class="fill {tone}" style:inline-size="{clamped * 100}%"></div>
  </div>
</div>

<style>
  .meter {
    margin-block-end: 0.9rem;
  }
  .row {
    display: flex;
    justify-content: space-between;
    gap: 1rem;
    font-size: 0.9rem;
    margin-block-end: 0.3rem;
  }
  .track {
    block-size: 0.5rem;
    border-radius: 999px;
    background: var(--surface-2);
    overflow: hidden;
  }
  .fill {
    block-size: 100%;
    border-radius: 999px;
    transition: inline-size 0.4s ease;
  }
  .fill.ok {
    background: var(--ok);
  }
  .fill.warn {
    background: var(--warn);
  }
  .fill.danger {
    background: var(--danger);
  }
</style>
