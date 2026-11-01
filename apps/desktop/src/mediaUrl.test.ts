import { beforeEach, describe, expect, it, vi } from "vitest";

const invokeMock = vi.fn<(cmd: string, args?: unknown) => Promise<unknown>>();
vi.mock("@tauri-apps/api/core", () => ({
  invoke: (cmd: string, args?: unknown) => invokeMock(cmd, args),
}));

import { forgetMediaUrl, getMediaUrl, resetMediaUrlCache } from "./mediaUrl";

describe("mediaUrl", () => {
  beforeEach(() => {
    resetMediaUrlCache();
    invokeMock.mockReset();
  });

  it("caches successful requests by path", async () => {
    invokeMock.mockResolvedValueOnce("asset://localhost/audio.wav");

    const p1 = getMediaUrl("/path/to/audio.wav");
    const p2 = getMediaUrl("/path/to/audio.wav");

    expect(p1).toBe(p2);
    const url = await p1;
    expect(url).toBe("asset://localhost/audio.wav");
    expect(invokeMock).toHaveBeenCalledTimes(1);
  });

  it("evicts from cache on failure", async () => {
    invokeMock.mockRejectedValueOnce(new Error("IPC failed"));

    await expect(getMediaUrl("/bad/path.wav")).rejects.toThrow("IPC failed");

    invokeMock.mockResolvedValueOnce("asset://localhost/retry.wav");
    const retryUrl = await getMediaUrl("/bad/path.wav");
    expect(retryUrl).toBe("asset://localhost/retry.wav");
    expect(invokeMock).toHaveBeenCalledTimes(2);
  });

  it("does not evict a superseded request when an older request rejects", async () => {
    let rejectFirst!: (err: Error) => void;
    const firstPromise = new Promise<string>((_, reject) => {
      rejectFirst = reject;
    });

    invokeMock.mockReturnValueOnce(firstPromise);
    const p1 = getMediaUrl("/race/path.wav");

    forgetMediaUrl("/race/path.wav");

    invokeMock.mockResolvedValueOnce("asset://localhost/second.wav");
    const p2 = getMediaUrl("/race/path.wav");
    expect(p2).not.toBe(p1);

    rejectFirst(new Error("late failure"));
    await expect(p1).rejects.toThrow("late failure");

    const p3 = getMediaUrl("/race/path.wav");
    expect(p3).toBe(p2);
    const secondResult = await p3;
    expect(secondResult).toBe("asset://localhost/second.wav");
  });
});
