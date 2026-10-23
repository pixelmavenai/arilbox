<script lang="ts">
  import { t } from '../lib/i18n';
  import { link } from '../lib/router.svelte';
</script>

<div class="card empty">
  <h1>{t('not_found.title')}</h1>
  <a href="/" use:link>{t('not_found.home')}</a>
</div>
