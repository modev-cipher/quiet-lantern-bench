import { useCallback, useEffect, useState } from "react";
import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";
import { openUrl } from "@tauri-apps/plugin-opener";

import type { UpdateSnapshot } from "./appUpdateModel";

type UpdateCommand =
  "update_check_now" | "update_install" | "update_cancel" | "update_dismiss";

/** Live updater snapshot: initial `update_status`, then every `update-state` event. */
export function useAppUpdate() {
  const [snapshot, setSnapshot] = useState<UpdateSnapshot | null>(null);

  useEffect(() => {
    let alive = true;
    invoke<UpdateSnapshot | null>("update_status")
      .then((s) => {
        if (alive) setSnapshot(s);
      })
      .catch(console.error);
    const unlisten = listen<UpdateSnapshot>("update-state", (event) => {
      if (alive) setSnapshot(event.payload);
    });
    return () => {
      alive = false;
      unlisten.then((stop) => stop()).catch(() => undefined);
    };
  }, []);

  const call = useCallback((command: UpdateCommand) => {
    invoke<UpdateSnapshot | null>(command)
      .then((s) => {
        if (s) setSnapshot(s);
      })
      .catch(console.error);
  }, []);

  const openRelease = useCallback(() => {
    const url = snapshot?.release_url;
    if (url) void openUrl(url).catch(console.error);
  }, [snapshot]);

  return {
    snapshot,
    checkNow: () => call("update_check_now"),
    install: () => call("update_install"),
    cancel: () => call("update_cancel"),
    dismiss: () => call("update_dismiss"),
    openRelease,
  };
}
