export const HARDWARE_REFRESH_INTERVAL_MS = 60_000;

type Listener = () => void;

export interface RefreshTargets {
  window: {
    addEventListener: (type: "focus", cb: Listener) => void;
    removeEventListener: (type: "focus", cb: Listener) => void;
  };
  document: {
    addEventListener: (type: "visibilitychange", cb: Listener) => void;
    removeEventListener: (type: "visibilitychange", cb: Listener) => void;
    readonly visibilityState: string;
  };
  setInterval: (cb: Listener, ms: number) => ReturnType<typeof setInterval>;
  clearInterval: (id: ReturnType<typeof setInterval>) => void;
}

function browserTargets(): RefreshTargets {
  return {
    window,
    document,
    setInterval: (cb, ms) => setInterval(cb, ms),
    clearInterval: (id) => clearInterval(id),
  };
}

export function subscribeHardwareProfile<T>(
  fetchProfile: () => Promise<T>,
  onProfile: (profile: T) => void,
  targets: RefreshTargets = browserTargets(),
): () => void {
  let active = true;
  let latestRequest = 0;
  const refresh = () => {
    const request = ++latestRequest;
    fetchProfile()
      .then((profile) => {
        if (active && request === latestRequest) onProfile(profile);
      })
      .catch(console.error);
  };
  const onVisibility = () => {
    if (targets.document.visibilityState === "visible") refresh();
  };
  refresh();
  const timer = targets.setInterval(refresh, HARDWARE_REFRESH_INTERVAL_MS);
  targets.window.addEventListener("focus", refresh);
  targets.document.addEventListener("visibilitychange", onVisibility);
  return () => {
    active = false;
    targets.clearInterval(timer);
    targets.window.removeEventListener("focus", refresh);
    targets.document.removeEventListener("visibilitychange", onVisibility);
  };
}
