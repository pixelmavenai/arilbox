// All user-visible text goes through t() (SPEC.md §15). English only in v1; adding a language
// means adding <lang>.json, a picker and dir="rtl" handling.
import en from '../i18n/en.json';

type Params = Record<string, string | number>;

const messages: Record<string, string> = en;

/** The UI locale, used for number and date formatting. */
export const locale = 'en';

/** The translation of `key`, with `{name}` placeholders filled from `params`. */
export function t(key: string, params?: Params): string {
  const text = messages[key];
  if (text === undefined) {
    if (import.meta.env.DEV) console.warn(`i18n: missing key ${key}`);
    return key;
  }
  if (!params) return text;
  return text.replace(/\{(\w+)\}/g, (match, name: string) =>
    name in params ? String(params[name]) : match,
  );
}

/** Whether `key` has a translation (for codes coming from the server). */
export function has(key: string): boolean {
  return key in messages;
}

/** `t(key)` if it exists, else `fallback` (e.g. the server's English message). */
export function tOr(key: string, fallback: string, params?: Params): string {
  return has(key) ? t(key, params) : fallback;
}

const units = ['byte', 'kilobyte', 'megabyte', 'gigabyte', 'terabyte', 'petabyte'] as const;

/** Bytes in binary steps (1024), labelled with the locale's unit names. */
export function formatBytes(bytes: number): string {
  let value = bytes;
  let unit = 0;
  while (Math.abs(value) >= 1024 && unit < units.length - 1) {
    value /= 1024;
    unit += 1;
  }
  return new Intl.NumberFormat(locale, {
    style: 'unit',
    unit: units[unit],
    unitDisplay: 'short',
    maximumFractionDigits: value < 10 && unit > 0 ? 1 : 0,
  }).format(value);
}

/** `ratio` (0–1) as a percentage. */
export function formatPercent(ratio: number): string {
  return new Intl.NumberFormat(locale, { style: 'percent', maximumFractionDigits: 1 }).format(
    ratio,
  );
}

export function formatNumber(n: number, digits = 2): string {
  return new Intl.NumberFormat(locale, { maximumFractionDigits: digits }).format(n);
}

/** An RFC 3339 timestamp in the user's time zone. */
export function formatDate(iso: string | null | undefined): string {
  if (!iso) return '—';
  return new Intl.DateTimeFormat(locale, { dateStyle: 'medium', timeStyle: 'medium' }).format(
    new Date(iso),
  );
}

/** A duration like "3 d 4 h" or "5 min". */
export function formatDuration(totalSeconds: number): string {
  const s = Math.max(0, Math.floor(totalSeconds));
  const d = Math.floor(s / 86400);
  const h = Math.floor((s % 86400) / 3600);
  const m = Math.floor((s % 3600) / 60);
  const part = (value: number, unit: 'day' | 'hour' | 'minute' | 'second') =>
    new Intl.NumberFormat(locale, { style: 'unit', unit, unitDisplay: 'narrow' }).format(value);
  if (d > 0) return `${part(d, 'day')} ${part(h, 'hour')}`;
  if (h > 0) return `${part(h, 'hour')} ${part(m, 'minute')}`;
  if (m > 0) return part(m, 'minute');
  return part(s, 'second');
}
