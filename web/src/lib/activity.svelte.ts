// Jobs started from this browser, followed live until they finish. Pages reload their data
// when `activity.version` changes.
import { type Job, watchJob } from './api';

export interface Tracked {
  job: Job | null;
  id: string;
  /** What the user did, for the toast title. */
  title: string;
}

class Activity {
  items: Tracked[] = $state([]);
  /** Bumped whenever a tracked job finishes. */
  version = $state(0);

  /** Follows job `id`, calling `onUpdate` on every change; resolves with the finished job. */
  track(id: string, title: string, onUpdate?: (job: Job) => void): Promise<Job> {
    this.items.push({ id, title, job: null });
    return new Promise((resolve) => {
      const stop = watchJob(id, (job) => {
        const item = this.items.find((i) => i.id === id);
        if (item) item.job = job;
        onUpdate?.(job);
        if (job.finished_at) {
          stop();
          this.version += 1;
          resolve(job);
          if (job.status === 'succeeded') setTimeout(() => this.dismiss(id), 6000);
        }
      });
    });
  }

  dismiss(id: string): void {
    this.items = this.items.filter((i) => i.id !== id);
  }
}

export const activity = new Activity();
