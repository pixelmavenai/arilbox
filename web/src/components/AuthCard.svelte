<script lang="ts">
  import type { Snippet } from 'svelte';
  import { t } from '../lib/i18n';

  // Centered card for the setup and login pages.
  let { title, children }: { title: string; children: Snippet } = $props();
</script>

<div class="wrap">
  <div class="card">
    <div class="brand">
      <img src="/favicon.svg" alt="" width="32" height="32" />
      <span>{t('app.name')}</span>
    </div>
    <h1>{title}</h1>
    {@render children()}
  </div>
</div>

<style>
  .wrap {
    min-block-size: 100vh;
    display: grid;
    place-items: center;
    padding: 1rem;
  }
  .card {
    inline-size: min(26rem, 100%);
    padding: 1.75rem;
  }
  .brand {
    display: flex;
    align-items: center;
    gap: 0.6rem;
    font-weight: 700;
    font-size: 1.25rem;
    margin-block-end: 1.25rem;
  }
  h1 {
    font-size: 1.2rem;
  }
</style>
