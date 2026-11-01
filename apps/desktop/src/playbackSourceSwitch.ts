// Track-switch source custody (owner feedback F9, 2026-08-29). The webview
// reuses ONE <audio> element for every track; before this module, a switch
// overwrote `src` on a still-playing element and let the async URL fetch
// interleave, which on WebKitGTK orphans the old GStreamer pipeline —
// pipelines accumulated per switch until playback turned choppy and only a
// webview reload (which rebuilds the web process) recovered. The contract
// pinned here: the old source is detached (pause → drop src → load) BEFORE
// the new URL is awaited, at most one source is ever attached, and a stale
// URL resolution can never install or play.

export interface SwitchableAudio {
  src: string;
  pause(): void;
  removeAttribute(name: string): void;
  load(): void;
  play(): Promise<void>;
}

export interface PlaybackSource {
  id: string;
  path: string;
}

export function sourceKey(source: PlaybackSource): string {
  return `${source.id}\u0000${source.path}`;
}

export interface SourceSwitcherDeps {
  resolveUrl(path: string): Promise<string>;
  forgetUrl(path: string): void;
  shouldAutoplay(): boolean;
  onEngineError(message?: string): void;
}

export interface SourceSwitcher {
  /// Reconcile the element with the wanted source (null = shelf cleared).
  sync(source: PlaybackSource | null): void;
  /// The key of the source currently attached to the element, or null
  /// while detached or mid-switch. Event handlers use this to tell user
  /// intent from engine-internal churn: a pause event with no installed
  /// source is the switch detach, never the user.
  installedKey(): string | null;
  /// Whether any source is wanted (drives the error handler's relevance).
  wantsSource(): boolean;
  /// The monotonic install-attempt generation: bumped every time a source
  /// INSTALLS onto the element. play() rejections are ASYNC: an attempt's
  /// rejection can land after its source was detached and something else
  /// installed, and reporting it then would mark the healthy replacement
  /// as errored (Kritias checkpoint 1, finding 1). Path identity cannot
  /// express this — in A1 -> B -> A2 the stale A1 rejection carries the
  /// SAME path as the healthy A2 install (Kritias re-verdict) — so the
  /// identity of an attempt is the generation of the install it played
  /// against. Identity churn over an unchanged path re-keys the SAME
  /// install and deliberately does not bump the generation, so a real
  /// failure of the still-playing source still reports.
  installGeneration(): number;
  /// Whether a play() attempt captured at `generation` still speaks for
  /// the element: something is installed and no re-install happened since.
  playAttemptStillCurrent(generation: number): boolean;
}

/// What the play/pause reconciliation effect should do with the element.
///
/// The F9 storm (owner feedback R2-2, 2026-08-30, reproduced on the real
/// WebKitGTK stack): while a switch is mid-flight the element has no source,
/// but WebKitGTK keeps reporting the OLD url in `currentSrc` after
/// `removeAttribute("src") + load()`. Guarding reconciliation on element
/// state therefore let `play()` run against a source-less element; the
/// synchronous `play` event dispatched enginePlaying(true), the rejected
/// play() promise dispatched engineError (playing=false), and every store
/// flip re-ran the effect — a self-sustaining ~300Hz play/pause oscillation
/// that floods the GStreamer bus (measured: 3.3M undrained messages) and
/// chops all audio until a webview reload. Custody, not element state, is
/// the oracle: while installedKey() is null the switcher owns the element
/// and the reconciler must not touch it — with one exception: play intent
/// while nothing is installed AND nothing is in flight means a resolution
/// FAILED earlier (the error path clears the wanted path so re-selecting
/// retries). Without "retry" the play button would go dead after a failed
/// resolve: the store clears the error and shows playing, but no code path
/// re-drives the switch.
export function reconcileAction(
  playing: boolean,
  elementPaused: boolean,
  installedKey: string | null,
  wantsSource: boolean,
): "play" | "pause" | "none" | "retry" {
  if (installedKey == null) {
    return playing && !wantsSource ? "retry" : "none";
  }
  if (playing && elementPaused) return "play";
  if (!playing && !elementPaused) return "pause";
  return "none";
}

export function createSourceSwitcher(
  audio: SwitchableAudio,
  deps: SourceSwitcherDeps,
): SourceSwitcher {
  let loadedPath: string | null = null;
  let installed: string | null = null;
  // The key the CURRENT wanted source would install under. The resolution
  // closure reads this instead of its own captured key so item-identity
  // churn over an unchanged path (same file re-surfaced under a new item)
  // can never downgrade installedKey() to a stale key — that mismatch
  // would silently suppress listening-progress persistence.
  let wantedKey: string | null = null;
  // Bumped on every install (see installGeneration in the interface).
  let generation = 0;

  const detach = () => {
    audio.pause();
    audio.removeAttribute("src");
    audio.load();
  };

  return {
    installedKey: () => installed,
    wantsSource: () => loadedPath != null,
    installGeneration: () => generation,
    playAttemptStillCurrent: (attempt) =>
      installed != null && generation === attempt,
    sync(source) {
      if (source == null) {
        loadedPath = null;
        installed = null;
        wantedKey = null;
        detach();
        return;
      }
      const key = sourceKey(source);
      wantedKey = key;
      if (loadedPath === source.path) {
        // Re-install only when the element genuinely holds the source; a
        // resolution still in flight installs the (updated) wanted key
        // itself when it lands.
        if (installed != null) installed = key;
        return;
      }
      const path = source.path;
      loadedPath = path;
      installed = null;
      detach();
      deps
        .resolveUrl(path)
        .then((url) => {
          if (loadedPath !== path) return;
          audio.src = url;
          audio.load();
          installed = wantedKey;
          generation += 1;
          if (deps.shouldAutoplay()) {
            // The attempt's identity is the generation of THIS install:
            // its async rejection reports only if no re-install happened
            // since. Path comparison is not enough — in A1 -> B -> A2 a
            // stale A1 rejection carries the same path as healthy A2.
            const attempt = generation;
            void audio.play().catch(() => {
              if (installed != null && generation === attempt) {
                deps.onEngineError();
              }
            });
          }
        })
        .catch((cause: unknown) => {
          deps.forgetUrl(path);
          if (loadedPath === path) {
            // Clearing the path lets a later re-select of the same track
            // retry the resolution instead of faking an installed source.
            loadedPath = null;
            deps.onEngineError(typeof cause === "string" ? cause : undefined);
          }
        });
    },
  };
}
