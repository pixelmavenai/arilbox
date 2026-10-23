<script lang="ts">
  import { activity } from '../lib/activity.svelte';
  import { type CreateRequest, type Job, type PoolInfo, type TemplateInfo, api } from '../lib/api';
  import { t } from '../lib/i18n';
  import { link } from '../lib/router.svelte';
  import { CONTAINER_NAME, RESERVED_NAMES, errorText } from '../lib/util';
  import Dialog from './Dialog.svelte';
  import JobSteps from './JobSteps.svelte';

  // Create wizard: one form, then live progress of the create job.
  let { open = $bindable(false) }: { open?: boolean } = $props();

  let templates: TemplateInfo[] = $state([]);
  let pools: PoolInfo[] = $state([]);

  let name = $state('');
  let template = $state('');
  let storage = $state('');
  let cores: number | null = $state(null);
  let memoryMb: number | null = $state(null);
  let diskGb: number | null = $state(null);
  let nesting = $state(false);
  let autostart = $state(false);
  let start = $state(true);

  let error = $state('');
  let busy = $state(false);
  let job: Job | null = $state(null);
  let jobId = $state('');

  const nameError = $derived(
    name === ''
      ? ''
      : !CONTAINER_NAME.test(name)
        ? t('create.name.invalid')
        : RESERVED_NAMES.includes(name)
          ? t('create.name.reserved')
          : '',
  );
  const pool = $derived(pools.find((p) => p.name === storage));

  $effect(() => {
    if (!open) return;
    reset();
    Promise.all([api.templates(), api.storage()])
      .then(([tpl, st]) => {
        templates = tpl;
        pools = st;
        template = key(tpl.find((x) => x.default) ?? tpl[0]);
        storage = (st.find((p) => p.default && p.available) ?? st.find((p) => p.available))?.name ?? '';
      })
      .catch((e) => (error = errorText(e)));
  });

  function key(x: { distro: string; release: string } | undefined): string {
    return x ? `${x.distro}/${x.release}` : '';
  }

  function reset() {
    name = '';
    cores = memoryMb = diskGb = null;
    nesting = autostart = false;
    start = true;
    error = '';
    job = null;
    jobId = '';
  }

  async function submit(e: SubmitEvent) {
    e.preventDefault();
    if (!name || nameError) return;
    const [distro, release] = template.split('/');
    const limits: CreateRequest['limits'] = {};
    if (cores) limits.cores = cores;
    if (memoryMb) limits.memory_mb = memoryMb;
    if (diskGb) limits.disk_gb = diskGb;
    busy = true;
    error = '';
    try {
      const { job_id } = await api.createContainer({
        name,
        template: { distro, release },
        storage: storage || undefined,
        limits,
        nesting,
        autostart,
        start,
      });
      jobId = job_id;
      await activity.track(job_id, t('action.create.title', { name }), (j) => (job = j));
    } catch (err) {
      error = errorText(err);
    } finally {
      busy = false;
    }
  }
</script>

<Dialog bind:open title={t('create.title')}>
  {#if error}
    <div class="alert" role="alert">{error}</div>
  {/if}
  {#if jobId}
    {#if job}
      <JobSteps {job} />
      {#if job.status === 'succeeded'}
        <div class="alert ok">{t('create.done', { name })}</div>
      {/if}
    {:else}
      <p class="muted">{t('job.starting')}</p>
    {/if}
  {:else}
    <form id="create-form" onsubmit={submit} novalidate>
      <div class="field">
        <label for="c-name">{t('create.name')}</label>
        <input
          id="c-name"
          type="text"
          autocomplete="off"
          spellcheck="false"
          required
          maxlength="63"
          bind:value={name}
          aria-invalid={nameError ? 'true' : undefined}
          aria-describedby="c-name-hint"
        />
        <div class="hint" id="c-name-hint">{nameError || t('create.name.hint')}</div>
      </div>
      <div class="grid-2">
        <div class="field">
          <label for="c-template">{t('create.template')}</label>
          <select id="c-template" bind:value={template}>
            {#each templates as tpl (key(tpl))}
              <option value={key(tpl)}>{t(`template.${tpl.distro}`)} {tpl.release}</option>
            {/each}
          </select>
        </div>
        <div class="field">
          <label for="c-storage">{t('create.storage')}</label>
          <select id="c-storage" bind:value={storage}>
            {#each pools as p (p.name)}
              <option value={p.name} disabled={!p.available}>
                {p.name} ({p.type}){p.available ? '' : ` — ${t('create.storage.unavailable')}`}
              </option>
            {/each}
          </select>
        </div>
      </div>
      <fieldset>
        <legend>{t('create.limits')}</legend>
        <div class="grid-2">
          <div class="field">
            <label for="c-cores">{t('create.cores')}</label>
            <input id="c-cores" type="number" min="1" max="1024" placeholder={t('create.unlimited')} bind:value={cores} />
          </div>
          <div class="field">
            <label for="c-mem">{t('create.memory')}</label>
            <input id="c-mem" type="number" min="16" step="256" placeholder={t('create.unlimited')} bind:value={memoryMb} />
          </div>
          <div class="field">
            <label for="c-disk">{t('create.disk')}</label>
            <input id="c-disk" type="number" min="1" placeholder={t('create.unlimited')} bind:value={diskGb} />
            {#if pool?.type === 'dir' && diskGb}
              <div class="hint">{t('create.disk.dir_hint')}</div>
            {/if}
          </div>
        </div>
      </fieldset>
      <label class="check"><input type="checkbox" bind:checked={start} /> {t('create.start')}</label>
      <label class="check"><input type="checkbox" bind:checked={autostart} /> {t('create.autostart')}</label>
      <label class="check"><input type="checkbox" bind:checked={nesting} /> {t('create.nesting')}</label>
    </form>
  {/if}
  {#snippet footer()}
    {#if jobId}
      {#if job?.status === 'succeeded'}
        <a class="button" href="/containers/{name}" use:link onclick={() => (open = false)}>
          {t('create.open')}
        </a>
      {/if}
      <button type="button" onclick={() => (open = false)}>{t('action.close')}</button>
    {:else}
      <button type="button" onclick={() => (open = false)}>{t('action.cancel')}</button>
      <button type="submit" form="create-form" class="primary" disabled={busy || !name || !!nameError}>
        {t('action.create')}
      </button>
    {/if}
  {/snippet}
</Dialog>

<style>
  fieldset {
    border: 1px solid var(--border);
    border-radius: 6px;
    padding: 0.75rem 1rem 0;
    margin: 0 0 1rem;
  }
  legend {
    padding-inline: 0.3rem;
    font-weight: 600;
  }
  .check {
    margin-block-end: 0.5rem;
  }
</style>
