// Typed client for the Arilbox REST API (SPEC.md §7). Shapes mirror the Rust types.

export type ContainerState = 'stopped' | 'starting' | 'running' | 'stopping' | 'frozen' | 'error';

export interface TemplateRef {
  distro: string;
  release: string;
}

export interface ContainerInfo {
  name: string;
  state: ContainerState;
  ipv4: string[];
  ipv6: string[];
  pid: number | null;
  cpu_usage_ns: number;
  memory_bytes: number;
  created_at: string;
  template: TemplateRef;
  gpu: boolean;
  app_id: string | null;
}

export interface Limits {
  cores?: number;
  memory_mb?: number;
  swap_mb?: number;
  disk_gb?: number;
  pids_max?: number;
}

export interface CreateRequest {
  name: string;
  template?: TemplateRef;
  storage?: string;
  limits?: Limits;
  nesting?: boolean;
  autostart?: boolean;
  start?: boolean;
}

export interface HostInfo {
  hostname: string;
  os: string;
  kernel: string;
  uptime_s: number;
  cpu_count: number;
  memory_total_bytes: number;
  platform_version: string;
}

export interface HostStats {
  cpu_percent: number;
  memory_total_bytes: number;
  memory_used_bytes: number;
  swap_total_bytes: number;
  swap_used_bytes: number;
  load: [number, number, number];
  uptime_s: number;
}

export interface Capabilities {
  zfs: boolean;
  nvidia: boolean;
  kvm: boolean;
  cgroup_v2: boolean;
}

export interface PoolInfo {
  name: string;
  type: 'zfs' | 'dir';
  available: boolean;
  default: boolean;
}

export interface TemplateInfo extends TemplateRef {
  default: boolean;
}

export type JobStatus = 'queued' | 'running' | 'succeeded' | 'failed' | 'cancelled';
export type StepStatus =
  | 'pending'
  | 'running'
  | 'succeeded'
  | 'failed'
  | 'compensated'
  | 'compensation_failed';

export interface LogLine {
  at: string;
  message: string;
}

export interface Step {
  id: string;
  label: string;
  status: StepStatus;
  started_at: string | null;
  finished_at: string | null;
  log: LogLine[];
}

export interface JobError {
  code: string;
  message: string;
  step: string | null;
  rollback_incomplete: boolean;
}

export interface Job {
  id: string;
  kind: string;
  target: string;
  status: JobStatus;
  steps: Step[];
  error: JobError | null;
  created_at: string;
  started_at: string | null;
  finished_at: string | null;
}

export interface Accepted {
  job_id: string;
}

/** An error response (`{ "error": { "code", "message" } }`) or a network failure. */
export class ApiError extends Error {
  constructor(
    readonly status: number,
    readonly code: string,
    message: string,
    readonly retryAfterS: number | null = null,
  ) {
    super(message);
  }
}

let onUnauthenticated: () => void = () => {};

/** Called when a request fails with 401 (the session ended). */
export function setUnauthenticatedHandler(handler: () => void): void {
  onUnauthenticated = handler;
}

async function request<T>(method: string, path: string, body?: unknown): Promise<T> {
  const headers: Record<string, string> = {};
  if (method !== 'GET') headers['X-Arilbox-Request'] = '1';
  if (body !== undefined) headers['Content-Type'] = 'application/json';
  let res: Response;
  try {
    res = await fetch(`/api/v1${path}`, {
      method,
      headers,
      body: body === undefined ? undefined : JSON.stringify(body),
      credentials: 'same-origin',
    });
  } catch (e) {
    throw new ApiError(0, 'network_error', e instanceof Error ? e.message : String(e));
  }
  if (res.status === 204) return undefined as T;
  const data: unknown = await res.json().catch(() => null);
  if (!res.ok) {
    const err = (data as { error?: { code?: string; message?: string } } | null)?.error;
    const retry = Number(res.headers.get('Retry-After'));
    const error = new ApiError(
      res.status,
      err?.code ?? 'http_error',
      err?.message ?? res.statusText,
      Number.isFinite(retry) && retry > 0 ? retry : null,
    );
    if (res.status === 401 && error.code === 'unauthenticated') onUnauthenticated();
    throw error;
  }
  return data as T;
}

const enc = encodeURIComponent;

export const api = {
  setupStatus: () => request<{ required: boolean }>('GET', '/auth/setup'),
  setup: (token: string, username: string, password: string) =>
    request<{ username: string }>('POST', '/auth/setup', { token, username, password }),
  login: (username: string, password: string) =>
    request<{ username: string }>('POST', '/auth/login', { username, password }),
  logout: () => request<void>('POST', '/auth/logout'),
  me: () => request<{ username: string }>('GET', '/auth/me'),
  changePassword: (current: string, next: string) =>
    request<void>('POST', '/auth/password', { current, new: next }),

  host: () => request<HostInfo>('GET', '/host'),
  hostStats: () => request<HostStats>('GET', '/host/stats'),
  capabilities: () => request<Capabilities>('GET', '/host/capabilities'),
  storage: () => request<PoolInfo[]>('GET', '/storage'),
  templates: () => request<TemplateInfo[]>('GET', '/templates'),

  containers: () => request<ContainerInfo[]>('GET', '/containers'),
  container: (name: string) => request<ContainerInfo>('GET', `/containers/${enc(name)}`),
  createContainer: (req: CreateRequest) => request<Accepted>('POST', '/containers', req),
  deleteContainer: (name: string, force = false) =>
    request<Accepted>('DELETE', `/containers/${enc(name)}?force=${force}`),
  start: (name: string) => request<Accepted>('POST', `/containers/${enc(name)}/start`),
  stop: (name: string) => request<Accepted>('POST', `/containers/${enc(name)}/stop`),
  restart: (name: string) => request<Accepted>('POST', `/containers/${enc(name)}/restart`),

  jobs: (limit = 50) => request<Job[]>('GET', `/jobs?limit=${limit}`),
  job: (id: string) => request<Job>('GET', `/jobs/${enc(id)}`),
};

/** WebSocket URL of a container console. */
export function consoleUrl(name: string, rows: number, cols: number): string {
  const scheme = location.protocol === 'https:' ? 'wss:' : 'ws:';
  return `${scheme}//${location.host}/api/v1/containers/${enc(name)}/console?rows=${rows}&cols=${cols}`;
}

/**
 * Follows a job over SSE (`GET /jobs/{id}/events`), calling `onUpdate` with the job as it
 * changes. Returns a function that stops watching. The stream ends after `job_finished`.
 */
export function watchJob(id: string, onUpdate: (job: Job) => void): () => void {
  const source = new EventSource(`/api/v1/jobs/${enc(id)}/events`);
  let job: Job | null = null;
  const parse = (e: Event) => JSON.parse((e as MessageEvent<string>).data);
  const step = (stepId: string) => job?.steps.find((s) => s.id === stepId);
  const emit = () => {
    if (job) onUpdate(structuredClone(job));
  };

  source.addEventListener('snapshot', (e) => {
    job = parse(e) as Job;
    emit();
    if (job.finished_at) source.close();
  });
  source.addEventListener('step_started', (e) => {
    const s = step(parse(e).step);
    if (s) {
      s.status = 'running';
      s.started_at = new Date().toISOString();
    }
    if (job) job.status = 'running';
    emit();
  });
  source.addEventListener('log', (e) => {
    const data = parse(e) as { step: string; at: string; message: string };
    step(data.step)?.log.push({ at: data.at, message: data.message });
    emit();
  });
  source.addEventListener('step_finished', (e) => {
    const data = parse(e) as { step: string; status: StepStatus };
    const s = step(data.step);
    if (s) {
      s.status = data.status;
      s.finished_at = new Date().toISOString();
    }
    emit();
  });
  source.addEventListener('job_finished', (e) => {
    const data = parse(e) as { status: JobStatus; error: JobError | null };
    if (job) {
      job.status = data.status;
      job.error = data.error;
      job.finished_at = new Date().toISOString();
    }
    emit();
    source.close();
  });
  // The server closes the stream after job_finished; don't let EventSource reconnect then.
  source.onerror = () => {
    if (job?.finished_at) source.close();
  };
  return () => source.close();
}
