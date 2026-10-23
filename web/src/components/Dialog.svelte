<script lang="ts">
  import type { Snippet } from 'svelte';

  // Native modal <dialog>; `open` is two-way bound. Escape closes it.
  let {
    open = $bindable(false),
    title,
    children,
    footer,
  }: { open?: boolean; title: string; children: Snippet; footer?: Snippet } = $props();

  let dialog: HTMLDialogElement | undefined = $state();

  $effect(() => {
    if (!dialog) return;
    if (open && !dialog.open) dialog.showModal();
    if (!open && dialog.open) dialog.close();
  });
</script>

<dialog bind:this={dialog} onclose={() => (open = false)} aria-labelledby="dialog-title">
  {#if open}
    <div class="dialog-body">
      <h2 id="dialog-title">{title}</h2>
      {@render children()}
    </div>
    {#if footer}
      <div class="dialog-foot">{@render footer()}</div>
    {/if}
  {/if}
</dialog>
