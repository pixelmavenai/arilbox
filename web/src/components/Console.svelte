<script lang="ts">
  import { FitAddon } from '@xterm/addon-fit';
  import { Terminal } from '@xterm/xterm';
  import '@xterm/xterm/css/xterm.css';
  import { consoleUrl } from '../lib/api';
  import { t } from '../lib/i18n';

  // Root shell in a running container over the console WebSocket (SPEC.md §7.3): binary
  // frames carry terminal bytes, text frames carry resize messages.
  let { name }: { name: string } = $props();

  type Status = 'connecting' | 'connected' | 'closed';
  let status: Status = $state('connecting');
  let host: HTMLDivElement | undefined = $state();
  let attempt = $state(0);

  $effect(() => {
    if (!host) return;
    void attempt; // Reconnect when bumped.
    const term = new Terminal({
      cursorBlink: true,
      fontFamily: getComputedStyle(document.documentElement).getPropertyValue('--mono'),
      fontSize: 14,
      scrollback: 5000,
      theme: { background: '#16181d', foreground: '#e6e6e6' },
    });
    const fit = new FitAddon();
    term.loadAddon(fit);
    term.open(host);
    fit.fit();

    status = 'connecting';
    const ws = new WebSocket(consoleUrl(name, term.rows, term.cols));
    ws.binaryType = 'arraybuffer';
    const encoder = new TextEncoder();
    const send = (data: Uint8Array<ArrayBuffer> | string) => {
      if (ws.readyState === WebSocket.OPEN) ws.send(data);
    };

    ws.onopen = () => {
      status = 'connected';
      term.focus();
    };
    ws.onmessage = (e) => {
      if (e.data instanceof ArrayBuffer) term.write(new Uint8Array(e.data));
    };
    ws.onclose = () => {
      status = 'closed';
      term.write(`\r\n\x1b[2m${t('console.ended')}\x1b[0m\r\n`);
    };

    const subs = [
      term.onData((data) => send(encoder.encode(data))),
      term.onBinary((data) => send(Uint8Array.from(data, (c) => c.charCodeAt(0)))),
      term.onResize(({ rows, cols }) => send(JSON.stringify({ type: 'resize', rows, cols }))),
    ];
    const observer = new ResizeObserver(() => fit.fit());
    observer.observe(host);

    return () => {
      observer.disconnect();
      subs.forEach((s) => s.dispose());
      ws.onclose = null;
      ws.close();
      term.dispose();
    };
  });
</script>

<div class="console">
  <div class="bar">
    <span class="badge {status === 'connected' ? 'ok' : status === 'closed' ? '' : 'info'}">
      {t(`console.${status}`)}
    </span>
    {#if status === 'closed'}
      <button class="small" onclick={() => attempt++}>{t('console.reconnect')}</button>
    {/if}
  </div>
  <div class="term" bind:this={host}></div>
</div>

<style>
  .console {
    display: flex;
    flex-direction: column;
    gap: 0.5rem;
  }
  .bar {
    display: flex;
    align-items: center;
    gap: 0.5rem;
  }
  .term {
    block-size: min(32rem, 65vh);
    padding: 0.5rem;
    background: #16181d;
    border-radius: var(--radius);
    overflow: hidden;
  }
  /* The terminal is always left-to-right, even in an RTL UI. */
  .term :global(.xterm) {
    direction: ltr;
    block-size: 100%;
  }
</style>
