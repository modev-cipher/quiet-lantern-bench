const FIRST_RETRY_MS = 1_000;
const MAX_RETRY_MS = 30_000;

interface Failure {
  attempts: number;
  retryAt: number;
}

export class FetchBackoff {
  private readonly failures = new Map<string, Failure>();

  canTry(key: string, now: number): boolean {
    const failure = this.failures.get(key);
    return failure == null || now >= failure.retryAt;
  }

  failed(key: string, now: number): void {
    const attempts = (this.failures.get(key)?.attempts ?? 0) + 1;
    const delay = Math.min(FIRST_RETRY_MS * 2 ** (attempts - 1), MAX_RETRY_MS);
    this.failures.set(key, { attempts, retryAt: now + delay });
  }

  succeeded(key: string): void {
    this.failures.delete(key);
  }
}
