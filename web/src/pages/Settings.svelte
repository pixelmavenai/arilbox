<script lang="ts">
  import { api } from '../lib/api';
  import { t } from '../lib/i18n';
  import { session } from '../lib/session.svelte';
  import { type Theme, theme } from '../lib/theme.svelte';
  import { MIN_PASSWORD, errorText } from '../lib/util';

  let current = $state('');
  let next = $state('');
  let confirm = $state('');
  let error = $state('');
  let done = $state(false);
  let busy = $state(false);

  const problem = $derived(
    [...next].length < MIN_PASSWORD
      ? t('password.too_short', { min: MIN_PASSWORD })
      : next !== confirm
        ? t('password.mismatch')
        : '',
  );

  async function change(e: SubmitEvent) {
    e.preventDefault();
    if (problem) return;
    busy = true;
    error = '';
    done = false;
    try {
      await api.changePassword(current, next);
      done = true;
      current = next = confirm = '';
    } catch (err) {
      error = errorText(err);
    } finally {
      busy = false;
    }
  }

  const themes: Theme[] = ['system', 'light', 'dark'];
</script>

<div class="page-head"><h1>{t('settings.title')}</h1></div>

<div class="cards">
  <section class="card">
    <h2>{t('settings.password')}</h2>
    <p class="muted">{t('settings.password.intro', { user: session.user ?? '' })}</p>
    {#if error}
      <div class="alert" role="alert">{error}</div>
    {/if}
    {#if done}
      <div class="alert ok" role="status">{t('settings.password.done')}</div>
    {/if}
    <form onsubmit={change}>
      <div class="field">
        <label for="p-current">{t('settings.password.current')}</label>
        <input id="p-current" type="password" autocomplete="current-password" required bind:value={current} />
      </div>
      <div class="field">
        <label for="p-new">{t('settings.password.new')}</label>
        <input id="p-new" type="password" autocomplete="new-password" required bind:value={next} />
        <div class="hint">{t('password.rule', { min: MIN_PASSWORD })}</div>
      </div>
      <div class="field">
        <label for="p-confirm">{t('password.confirm')}</label>
        <input id="p-confirm" type="password" autocomplete="new-password" required bind:value={confirm} />
      </div>
      {#if next && confirm && problem}
        <p class="muted" role="status">{problem}</p>
      {/if}
      <button class="primary" type="submit" disabled={busy || !current || !!problem}>
        {t('settings.password.submit')}
      </button>
    </form>
  </section>

  <section class="card">
    <h2>{t('settings.appearance')}</h2>
    <fieldset class="themes">
      <legend>{t('settings.theme')}</legend>
      {#each themes as option (option)}
        <label class="check">
          <input
            type="radio"
            name="theme"
            value={option}
            checked={theme.current === option}
            onchange={() => theme.set(option)}
          />
          {t(`settings.theme.${option}`)}
        </label>
      {/each}
    </fieldset>
  </section>
</div>

<style>
  .themes {
    border: none;
    padding: 0;
    margin: 0;
  }
  .themes legend {
    font-weight: 500;
    margin-block-end: 0.4rem;
  }
</style>
