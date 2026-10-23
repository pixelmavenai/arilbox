<script lang="ts">
  import AuthCard from '../components/AuthCard.svelte';
  import { api } from '../lib/api';
  import { t } from '../lib/i18n';
  import { session } from '../lib/session.svelte';
  import { errorText } from '../lib/util';

  let username = $state('');
  let password = $state('');
  let error = $state('');
  let busy = $state(false);

  async function submit(e: SubmitEvent) {
    e.preventDefault();
    busy = true;
    error = '';
    try {
      const res = await api.login(username, password);
      session.loggedIn(res.username);
    } catch (err) {
      error = errorText(err);
      password = '';
    } finally {
      busy = false;
    }
  }
</script>

<AuthCard title={t('auth.login.title')}>
  {#if error}
    <div class="alert" role="alert">{error}</div>
  {/if}
  <form onsubmit={submit}>
    <div class="field">
      <label for="l-user">{t('auth.username')}</label>
      <!-- svelte-ignore a11y_autofocus -->
      <input id="l-user" type="text" autocomplete="username" required autofocus bind:value={username} />
    </div>
    <div class="field">
      <label for="l-pass">{t('auth.password')}</label>
      <input id="l-pass" type="password" autocomplete="current-password" required bind:value={password} />
    </div>
    <button class="primary" type="submit" disabled={busy || !username || !password}>
      {t('auth.login')}
    </button>
  </form>
</AuthCard>
