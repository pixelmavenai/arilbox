<script lang="ts">
  import AuthCard from '../components/AuthCard.svelte';
  import { api } from '../lib/api';
  import { t } from '../lib/i18n';
  import { router } from '../lib/router.svelte';
  import { session } from '../lib/session.svelte';
  import { MIN_PASSWORD, USERNAME, errorText } from '../lib/util';

  // First-run setup: the token comes from the setup URL in the arilboxd log.
  let token = $state(router.route.query.get('token') ?? '');
  let username = $state('admin');
  let password = $state('');
  let confirm = $state('');
  let error = $state('');
  let busy = $state(false);

  const problems = $derived.by(() => {
    if (!USERNAME.test(username)) return t('setup.username.invalid');
    if ([...password].length < MIN_PASSWORD) return t('password.too_short', { min: MIN_PASSWORD });
    if (password !== confirm) return t('password.mismatch');
    return '';
  });

  async function submit(e: SubmitEvent) {
    e.preventDefault();
    if (problems || !token) return;
    busy = true;
    error = '';
    try {
      const res = await api.setup(token.trim(), username, password);
      session.loggedIn(res.username);
    } catch (err) {
      error = errorText(err);
    } finally {
      busy = false;
    }
  }
</script>

<AuthCard title={t('setup.title')}>
  <p class="muted">{t('setup.intro')}</p>
  {#if error}
    <div class="alert" role="alert">{error}</div>
  {/if}
  <form onsubmit={submit}>
    <div class="field">
      <label for="s-token">{t('setup.token')}</label>
      <input id="s-token" type="text" autocomplete="off" spellcheck="false" required bind:value={token} />
      <div class="hint">{t('setup.token.hint')}</div>
    </div>
    <div class="field">
      <label for="s-user">{t('auth.username')}</label>
      <input id="s-user" type="text" autocomplete="username" required bind:value={username} />
    </div>
    <div class="field">
      <label for="s-pass">{t('auth.password')}</label>
      <input id="s-pass" type="password" autocomplete="new-password" required bind:value={password} />
      <div class="hint">{t('password.rule', { min: MIN_PASSWORD })}</div>
    </div>
    <div class="field">
      <label for="s-confirm">{t('password.confirm')}</label>
      <input id="s-confirm" type="password" autocomplete="new-password" required bind:value={confirm} />
    </div>
    {#if password && confirm && problems}
      <p class="muted" role="status">{problems}</p>
    {/if}
    <button class="primary" type="submit" disabled={busy || !!problems || !token}>
      {t('setup.submit')}
    </button>
  </form>
</AuthCard>
