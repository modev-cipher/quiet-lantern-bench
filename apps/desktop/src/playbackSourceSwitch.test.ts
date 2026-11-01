import { describe, expect, it } from "vitest";

import {
  createSourceSwitcher,
  reconcileAction,
  sourceKey,
  type SwitchableAudio,
} from "./playbackSourceSwitch";

// F9 regression pins. The webview holds one <audio> element for all
// tracks; the leak class this guards against is per-switch pipeline
// accumulation: a source overwritten while attached (or a stale async URL
// landing after a newer switch) orphans a decode pipeline the web process
// never releases, and playback degrades until a full webview reload. The
// stub models pipeline custody: attach on src assignment, release only by
// the detach sequence (pause → src removed → load).

interface Deferred {
  promise: Promise<string>;
  resolve(url: string): void;
  reject(cause: unknown): void;
}

function deferred(): Deferred {
  let resolve!: (url: string) => void;
  let reject!: (cause: unknown) => void;
  const promise = new Promise<string>((res, rej) => {
    resolve = res;
    reject = rej;
  });
  return { promise, resolve, reject };
}

class AudioStub implements SwitchableAudio {
  src = "";
  attached: string | null = null;
  livePipelines = 0;
  maxLivePipelines = 0;
  playedUrls: string[] = [];
  ops: string[] = [];
  private srcAttribute: string | null = null;

  pause(): void {
    this.ops.push("pause");
  }

  removeAttribute(name: string): void {
    this.ops.push(`removeAttribute:${name}`);
    if (name === "src") this.srcAttribute = null;
  }

  load(): void {
    this.ops.push("load");
    if (this.attached != null && this.srcAttribute == null) {
      this.attached = null;
      this.livePipelines -= 1;
    } else if (
      this.srcAttribute != null &&
      this.attached !== this.srcAttribute
    ) {
      this.attached = this.srcAttribute;
      this.livePipelines += 1;
      this.maxLivePipelines = Math.max(
        this.maxLivePipelines,
        this.livePipelines,
      );
    }
  }

  play(): Promise<void> {
    this.ops.push("play");
    if (this.attached != null) this.playedUrls.push(this.attached);
    return Promise.resolve();
  }

  assign(url: string): void {
    this.srcAttribute = url;
    this.ops.push(`src:${url}`);
  }
}

function harness(options?: { autoplay?: boolean }) {
  const audio = new AudioStub();
  const proxied = new Proxy(audio, {
    set(target, prop, value) {
      if (prop === "src") {
        target.assign(value as string);
        target.src = value as string;
        return true;
      }
      return Reflect.set(target, prop, value);
    },
  });
  const pending = new Map<string, Deferred>();
  const forgotten: string[] = [];
  const errors: (string | undefined)[] = [];
  const switcher = createSourceSwitcher(proxied, {
    resolveUrl: (path) => {
      const entry = deferred();
      pending.set(path, entry);
      return entry.promise;
    },
    forgetUrl: (path) => forgotten.push(path),
    shouldAutoplay: () => options?.autoplay ?? true,
    onEngineError: (message) => errors.push(message),
  });
  return { audio, switcher, pending, forgotten, errors };
}

async function settle(): Promise<void> {
  await Promise.resolve();
  await Promise.resolve();
}

describe("track-switch source custody", () => {
  it("detaches the old source before the new URL is even requested", () => {
    const { audio, switcher, pending } = harness();
    switcher.sync({ id: "a", path: "/books/a.mp3" });
    pending.get("/books/a.mp3")!.resolve("http://127.0.0.1:1/media/a");
    switcher.sync({ id: "b", path: "/books/b.mp3" });
    const detachAt = audio.ops.indexOf("pause");
    expect(detachAt).toBeGreaterThanOrEqual(0);
    expect(audio.ops[detachAt + 1]).toBe("removeAttribute:src");
    expect(audio.ops[detachAt + 2]).toBe("load");
  });

  it("soak: 20 rapid switches never hold more than one live pipeline and only the last source plays", async () => {
    const { audio, switcher, pending } = harness();
    const paths: string[] = [];
    for (let i = 0; i < 20; i += 1) {
      const path = `/books/track-${i}.mp3`;
      paths.push(path);
      switcher.sync({ id: `t${i}`, path });
    }
    // URLs resolve late and OUT OF ORDER — every stale resolution must be
    // dropped without attaching, playing, or touching the element.
    for (const path of [...paths].reverse()) {
      pending.get(path)!.resolve(`http://127.0.0.1:1/media/${path}`);
    }
    await settle();
    expect(audio.maxLivePipelines).toBeLessThanOrEqual(1);
    expect(audio.livePipelines).toBe(1);
    expect(audio.attached).toBe("http://127.0.0.1:1/media//books/track-19.mp3");
    expect(audio.playedUrls).toEqual([
      "http://127.0.0.1:1/media//books/track-19.mp3",
    ]);
    expect(switcher.installedKey()).toBe(
      sourceKey({ id: "t19", path: "/books/track-19.mp3" }),
    );
  });

  it("marks the source installed only after the element holds it", async () => {
    const { switcher, pending } = harness();
    switcher.sync({ id: "a", path: "/books/a.mp3" });
    expect(switcher.installedKey()).toBeNull();
    pending.get("/books/a.mp3")!.resolve("http://127.0.0.1:1/media/a");
    await settle();
    expect(switcher.installedKey()).toBe(
      sourceKey({ id: "a", path: "/books/a.mp3" }),
    );
  });

  it("re-selecting the already-loaded path re-installs without detaching", async () => {
    const { audio, switcher, pending } = harness();
    switcher.sync({ id: "a", path: "/books/a.mp3" });
    pending.get("/books/a.mp3")!.resolve("http://127.0.0.1:1/media/a");
    await settle();
    const opsBefore = audio.ops.length;
    switcher.sync({ id: "a2", path: "/books/a.mp3" });
    expect(audio.ops.length).toBe(opsBefore);
    expect(switcher.installedKey()).toBe(
      sourceKey({ id: "a2", path: "/books/a.mp3" }),
    );
  });

  it("clearing the shelf detaches and releases the pipeline", async () => {
    const { audio, switcher, pending } = harness();
    switcher.sync({ id: "a", path: "/books/a.mp3" });
    pending.get("/books/a.mp3")!.resolve("http://127.0.0.1:1/media/a");
    await settle();
    expect(audio.livePipelines).toBe(1);
    switcher.sync(null);
    expect(audio.livePipelines).toBe(0);
    expect(switcher.installedKey()).toBeNull();
    expect(switcher.wantsSource()).toBe(false);
  });

  it("a failed URL for the CURRENT source reports and forgets; a stale failure only forgets", async () => {
    const { switcher, pending, forgotten, errors } = harness();
    switcher.sync({ id: "a", path: "/books/a.mp3" });
    switcher.sync({ id: "b", path: "/books/b.mp3" });
    pending.get("/books/a.mp3")!.reject("gone");
    pending.get("/books/b.mp3")!.reject("locked");
    await settle();
    expect(forgotten).toEqual(["/books/a.mp3", "/books/b.mp3"]);
    expect(errors).toEqual(["locked"]);
  });

  it("identity churn over an unchanged path installs the NEW key when resolution lands", async () => {
    const { switcher, pending } = harness();
    switcher.sync({ id: "old", path: "/books/a.mp3" });
    // Mid-flight, the same file re-surfaces under a new item identity: the
    // fast path must not fake an install, and the resolution must install
    // the updated key — a stale key here silently suppresses saveListen.
    switcher.sync({ id: "new", path: "/books/a.mp3" });
    expect(switcher.installedKey()).toBeNull();
    pending.get("/books/a.mp3")!.resolve("http://127.0.0.1:1/media/a");
    await settle();
    expect(switcher.installedKey()).toBe(
      sourceKey({ id: "new", path: "/books/a.mp3" }),
    );
  });

  it("a failed resolution clears the wanted path so re-selecting the track retries", async () => {
    const { switcher, pending, errors } = harness();
    const resolutions: string[] = [];
    switcher.sync({ id: "a", path: "/books/a.mp3" });
    resolutions.push("first");
    pending.get("/books/a.mp3")!.reject("locked");
    await settle();
    expect(errors).toEqual(["locked"]);
    expect(switcher.wantsSource()).toBe(false);
    expect(switcher.installedKey()).toBeNull();
    switcher.sync({ id: "a", path: "/books/a.mp3" });
    const retry = pending.get("/books/a.mp3")!;
    retry.resolve("http://127.0.0.1:1/media/a");
    await settle();
    expect(switcher.installedKey()).toBe(
      sourceKey({ id: "a", path: "/books/a.mp3" }),
    );
  });

  it("does not autoplay when playback intent is off", async () => {
    const { audio, switcher, pending } = harness({ autoplay: false });
    switcher.sync({ id: "a", path: "/books/a.mp3" });
    pending.get("/books/a.mp3")!.resolve("http://127.0.0.1:1/media/a");
    await settle();
    expect(audio.playedUrls).toEqual([]);
    expect(audio.attached).toBe("http://127.0.0.1:1/media/a");
  });
});

// R2-2 regression pins (owner feedback 2026-08-30): the reconciliation
// oscillator. Reproduced on the real WebKitGTK stack: after detach the
// element keeps a STALE currentSrc, so any element-state guard lets the
// play/pause reconciler run mid-switch; play() on a source-less element
// fires a synchronous `play` event (store: playing=true) and then rejects
// (store: error, playing=false), and every store flip re-runs the
// reconciler — a self-sustaining ~300Hz play/pause storm that floods the
// GStreamer bus and chops all playback until a webview reload. Custody
// (installedKey) is the only oracle that is null exactly while the
// switcher owns the element.
describe("reconcileAction", () => {
  it("never touches the element while a switch is in flight (the storm trigger)", () => {
    // playing intent + element paused + mid-switch: the pre-fix reconciler
    // called play() here; this is the exact state the storm ignites from.
    expect(reconcileAction(true, true, null, true)).toBe("none");
    expect(reconcileAction(false, false, null, true)).toBe("none");
    expect(reconcileAction(true, false, null, true)).toBe("none");
    expect(reconcileAction(false, true, null, true)).toBe("none");
  });

  it("retries the switch when play intent lands after a failed resolution", () => {
    // Nothing installed AND nothing in flight = a resolution failed and was
    // forgotten; without retry the play button is dead over a "playing" UI.
    expect(reconcileAction(true, true, null, false)).toBe("retry");
    expect(reconcileAction(true, false, null, false)).toBe("retry");
    expect(reconcileAction(false, true, null, false)).toBe("none");
    expect(reconcileAction(false, false, null, false)).toBe("none");
  });

  it("reconciles normally once a source is installed", () => {
    const key = sourceKey({ id: "a", path: "/books/a.mp3" });
    expect(reconcileAction(true, true, key, true)).toBe("play");
    expect(reconcileAction(false, false, key, true)).toBe("pause");
    expect(reconcileAction(true, false, key, true)).toBe("none");
    expect(reconcileAction(false, true, key, true)).toBe("none");
  });

  it("stays out through a full rapid-switch sequence until the last install", async () => {
    const { switcher, pending } = harness();
    for (const id of ["a", "b", "c"]) {
      switcher.sync({ id, path: `/books/${id}.mp3` });
      // Mid-flight after every switch: reconciler must be a no-op even
      // though play intent is on and the element reports paused.
      expect(
        reconcileAction(
          true,
          true,
          switcher.installedKey(),
          switcher.wantsSource(),
        ),
      ).toBe("none");
    }
    pending.get("/books/c.mp3")!.resolve("http://127.0.0.1:1/media/c");
    await settle();
    expect(
      reconcileAction(
        true,
        true,
        switcher.installedKey(),
        switcher.wantsSource(),
      ),
    ).toBe("play");
  });

  it("a failed resolution leaves exactly the retry state, and retrying re-resolves", async () => {
    const { switcher, pending, errors } = harness();
    switcher.sync({ id: "a", path: "/books/a.mp3" });
    pending.get("/books/a.mp3")!.reject("server down");
    await settle();
    expect(errors).toEqual(["server down"]);
    // The dead-play-button state: no source installed, nothing in flight.
    expect(
      reconcileAction(
        true,
        true,
        switcher.installedKey(),
        switcher.wantsSource(),
      ),
    ).toBe("retry");
    // The retry re-drives sync, which re-requests the URL.
    switcher.sync({ id: "a", path: "/books/a.mp3" });
    pending.get("/books/a.mp3")!.resolve("http://127.0.0.1:1/media/a");
    await settle();
    expect(switcher.installedKey()).toBe(
      sourceKey({ id: "a", path: "/books/a.mp3" }),
    );
  });

  it("installs custody BEFORE autoplay so the synchronous play event is attributable", async () => {
    // Load-bearing ordering for the onPlay guard: if play() ran before
    // installed was set, the autoplay's enginePlaying(true) would be
    // swallowed and the UI would sit on "paused" over audible playback.
    const { audio, switcher, pending } = harness();
    const custodyAtPlay: (string | null)[] = [];
    const originalPlay = audio.play.bind(audio);
    audio.play = () => {
      custodyAtPlay.push(switcher.installedKey());
      return originalPlay();
    };
    switcher.sync({ id: "a", path: "/books/a.mp3" });
    pending.get("/books/a.mp3")!.resolve("http://127.0.0.1:1/media/a");
    await settle();
    expect(custodyAtPlay).toEqual([
      sourceKey({ id: "a", path: "/books/a.mp3" }),
    ]);
  });
});

// Kritias checkpoint 1, finding 1: a play() rejection is ASYNC and can land
// after its source was detached and a replacement installed. Reporting it
// then marks the healthy replacement as errored (and, via engineError's
// playing=false, can stop it). Both rejection paths — the switcher's
// autoplay and the reconciler's play — must test attempt identity first.
describe("stale play() rejection custody", () => {
  function playControlledHarness() {
    const base = harness();
    const playAttempts: {
      reject(cause: unknown): void;
      resolve(): void;
    }[] = [];
    base.audio.play = () => {
      base.audio.ops.push("play");
      if (base.audio.attached != null)
        base.audio.playedUrls.push(base.audio.attached);
      return new Promise<void>((resolve, reject) => {
        playAttempts.push({ resolve, reject });
      });
    };
    return { ...base, playAttempts };
  }

  it("A's stale autoplay rejection after B installs must not report an error", async () => {
    const { switcher, pending, errors, playAttempts } = playControlledHarness();
    switcher.sync({ id: "a", path: "/books/a.mp3" });
    pending.get("/books/a.mp3")!.resolve("http://127.0.0.1:1/media/a");
    await settle();
    expect(playAttempts.length).toBe(1);
    // The user switches; B installs and plays.
    switcher.sync({ id: "b", path: "/books/b.mp3" });
    pending.get("/books/b.mp3")!.resolve("http://127.0.0.1:1/media/b");
    await settle();
    expect(switcher.installedKey()).toBe(
      sourceKey({ id: "b", path: "/books/b.mp3" }),
    );
    // A's play() rejection lands late (the detach aborted it).
    playAttempts[0]!.reject(new Error("AbortError"));
    await settle();
    expect(errors).toEqual([]);
    expect(switcher.installedKey()).toBe(
      sourceKey({ id: "b", path: "/books/b.mp3" }),
    );
  });

  it("a rejection while the SAME source still owns the element still reports", async () => {
    const { switcher, pending, errors, playAttempts } = playControlledHarness();
    switcher.sync({ id: "a", path: "/books/a.mp3" });
    pending.get("/books/a.mp3")!.resolve("http://127.0.0.1:1/media/a");
    await settle();
    playAttempts[0]!.reject(new Error("NotAllowedError"));
    await settle();
    expect(errors.length).toBe(1);
  });

  it("playAttemptStillCurrent: identity churn keeps the attempt, a switch ends it", async () => {
    const { switcher, pending } = playControlledHarness();
    switcher.sync({ id: "a", path: "/books/a.mp3" });
    pending.get("/books/a.mp3")!.resolve("http://127.0.0.1:1/media/a");
    await settle();
    const attempt = switcher.installGeneration();
    expect(switcher.playAttemptStillCurrent(attempt)).toBe(true);
    // Same file re-surfaces under a new item identity: the attempt still
    // speaks for the audible element (same install, generation unchanged).
    switcher.sync({ id: "a2", path: "/books/a.mp3" });
    expect(switcher.playAttemptStillCurrent(attempt)).toBe(true);
    // A real switch: the attempt no longer owns the element — both while
    // detached (installed null)...
    switcher.sync({ id: "b", path: "/books/b.mp3" });
    expect(switcher.playAttemptStillCurrent(attempt)).toBe(false);
    // ...and, critically, after the NEXT install lands: installed is
    // non-null again but the generation has advanced, so the old attempt
    // must still be dead (a guard that only checks installed-ness would
    // pass here — that is the exact decoration this line kills).
    pending.get("/books/b.mp3")!.resolve("http://127.0.0.1:1/media/b");
    await settle();
    expect(switcher.installedKey()).not.toBeNull();
    expect(switcher.playAttemptStillCurrent(attempt)).toBe(false);
    expect(switcher.playAttemptStillCurrent(-1)).toBe(false);
  });

  it("A1 -> B -> A2: A1's stale rejection must not error the healthy A2 re-install of the SAME path (Kritias re-verdict)", async () => {
    const { switcher, pending, errors, playAttempts } = playControlledHarness();
    // A installs and plays (attempt 1 pending).
    switcher.sync({ id: "a", path: "/books/a.mp3" });
    pending.get("/books/a.mp3")!.resolve("http://127.0.0.1:1/media/a");
    await settle();
    expect(playAttempts.length).toBe(1);
    // Switch away to B (attempt 2 pending)...
    switcher.sync({ id: "b", path: "/books/b.mp3" });
    pending.get("/books/b.mp3")!.resolve("http://127.0.0.1:1/media/b");
    await settle();
    expect(playAttempts.length).toBe(2);
    // ...and back to the SAME path A (fresh resolution, attempt 3 pending).
    switcher.sync({ id: "a", path: "/books/a.mp3" });
    pending.get("/books/a.mp3")!.resolve("http://127.0.0.1:1/media/a");
    await settle();
    expect(playAttempts.length).toBe(3);
    // A1's and B's stale rejections land late: path identity would let A1
    // through (same path as the current install); generation must not.
    playAttempts[0]!.reject(new Error("AbortError"));
    playAttempts[1]!.reject(new Error("AbortError"));
    await settle();
    expect(errors).toEqual([]);
    expect(switcher.installedKey()).toBe(
      sourceKey({ id: "a", path: "/books/a.mp3" }),
    );
    // The CURRENT attempt's rejection is a real failure and still reports.
    playAttempts[2]!.reject(new Error("NotSupportedError"));
    await settle();
    expect(errors.length).toBe(1);
  });
});
