import { invoke } from "@tauri-apps/api/core";

const cache = new Map<string, Promise<string>>();

export function getMediaUrl(path: string): Promise<string> {
  let pending = cache.get(path);
  if (pending == null) {
    const request = invoke<string>("get_media_url", { path }).catch(
      (error: unknown) => {
        if (cache.get(path) === request) {
          cache.delete(path);
        }
        throw error;
      },
    );
    cache.set(path, request);
    pending = request;
  }
  return pending;
}

export function forgetMediaUrl(path: string): void {
  cache.delete(path);
}

export function resetMediaUrlCache(): void {
  cache.clear();
}
