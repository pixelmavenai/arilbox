import { ApiError } from './api';
import { t, tOr } from './i18n';

/** Text for an error from the API (translated by code) or anything else. */
export function errorText(e: unknown): string {
  if (e instanceof ApiError) {
    if (e.code === 'too_many_attempts' && e.retryAfterS) {
      return t('error.too_many_attempts_wait', { minutes: Math.ceil(e.retryAfterS / 60) });
    }
    return tOr(`error.${e.code}`, e.message);
  }
  return e instanceof Error ? e.message : String(e);
}

/** Container name rule from SPEC.md §5.2 (the server checks again). */
export const CONTAINER_NAME = /^[a-z](?:[a-z0-9-]{0,61}[a-z0-9])?$/;
export const RESERVED_NAMES = ['arilbox', 'host', 'default'];

/** Username rule (`^[a-z_][a-z0-9_-]{0,31}$`). */
export const USERNAME = /^[a-z_][a-z0-9_-]{0,31}$/;
export const MIN_PASSWORD = 10;

/** Text for a job kind, e.g. "Create container". */
export function jobKind(kind: string): string {
  return tOr(`job.kind.${kind}`, kind);
}
