/**
 * Calls `fn` now and then every `ms` while the page is visible (and once when it becomes
 * visible again). Returns a function that stops polling; use it as an `$effect` cleanup.
 */
export function poll(fn: () => unknown, ms: number): () => void {
  let timer: ReturnType<typeof setTimeout> | undefined;
  let stopped = false;
  const tick = async () => {
    if (stopped) return;
    if (document.visibilityState === 'visible') {
      try {
        await fn();
      } catch {
        // Callers show their own errors; keep polling.
      }
    }
    if (!stopped) timer = setTimeout(tick, ms);
  };
  const onVisible = () => {
    if (document.visibilityState === 'visible') {
      clearTimeout(timer);
      void tick();
    }
  };
  document.addEventListener('visibilitychange', onVisible);
  void tick();
  return () => {
    stopped = true;
    clearTimeout(timer);
    document.removeEventListener('visibilitychange', onVisible);
  };
}

/** CPU use in percent of one core, from successive `cpu_usage_ns` samples per key. */
export class CpuRate {
  private last = new Map<string, { ns: number; at: number }>();

  sample(key: string, ns: number, at = performance.now()): number | null {
    const prev = this.last.get(key);
    this.last.set(key, { ns, at });
    if (!prev || at <= prev.at || ns < prev.ns) return null;
    return ((ns - prev.ns) / ((at - prev.at) * 1e6)) * 100;
  }
}
