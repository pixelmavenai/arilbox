// A small History-API router. The daemon serves index.html for every non-API path.

export type RouteName =
  | 'setup'
  | 'login'
  | 'dashboard'
  | 'containers'
  | 'container'
  | 'jobs'
  | 'job'
  | 'settings'
  | 'not_found';

export interface Route {
  name: RouteName;
  params: Record<string, string>;
  query: URLSearchParams;
}

const table: [RouteName, RegExp, string[]][] = [
  ['dashboard', /^\/$/, []],
  ['setup', /^\/setup$/, []],
  ['login', /^\/login$/, []],
  ['containers', /^\/containers$/, []],
  ['container', /^\/containers\/([^/]+)$/, ['name']],
  ['jobs', /^\/jobs$/, []],
  ['job', /^\/jobs\/([^/]+)$/, ['id']],
  ['settings', /^\/settings$/, []],
];

function match(path: string, search: string): Route {
  const query = new URLSearchParams(search);
  for (const [name, re, keys] of table) {
    const m = re.exec(path);
    if (m) {
      const params: Record<string, string> = {};
      keys.forEach((k, i) => (params[k] = decodeURIComponent(m[i + 1])));
      return { name, params, query };
    }
  }
  return { name: 'not_found', params: {}, query };
}

class Router {
  path = $state(location.pathname);
  search = $state(location.search);
  route: Route = $derived(match(this.path, this.search));

  constructor() {
    window.addEventListener('popstate', () => this.sync());
  }

  sync() {
    this.path = location.pathname;
    this.search = location.search;
  }
}

export const router = new Router();

/** Goes to `to` (a path with optional query) without reloading. */
export function navigate(to: string, replace = false): void {
  if (replace) history.replaceState(null, '', to);
  else history.pushState(null, '', to);
  router.sync();
  window.scrollTo(0, 0);
}

/** `use:link` on an `<a href>`: plain left clicks navigate in-app. */
export function link(node: HTMLAnchorElement) {
  const onClick = (e: MouseEvent) => {
    if (e.defaultPrevented || e.button !== 0 || e.metaKey || e.ctrlKey || e.shiftKey || e.altKey) {
      return;
    }
    const url = new URL(node.href);
    if (url.origin !== location.origin || node.target) return;
    e.preventDefault();
    navigate(url.pathname + url.search);
  };
  node.addEventListener('click', onClick);
  return { destroy: () => node.removeEventListener('click', onClick) };
}
