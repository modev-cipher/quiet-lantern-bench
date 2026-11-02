import { useCallback, useEffect, useRef, useState } from "react";
import { invoke } from "@tauri-apps/api/core";

import type { FullCatalog, InstalledModel } from "./modelCatalogModel";
import { notifySetupChanged } from "./views";
import { commandErrorText } from "./commandError";

export type ModelAction =
  | { kind: "remove"; modelId: string }
  | { kind: "cancel"; modelId: string }
  | { kind: "default"; task: string; modelId: string | null };

interface ReadState<T> {
  data: T | null;
  loading: boolean;
  error: string | null;
}

function useModelRead<T>(command: string) {
  const [state, setState] = useState<ReadState<T>>({
    data: null,
    loading: true,
    error: null,
  });
  const latest = useRef(0);
  const active = useRef(false);
  const refresh = useCallback(async () => {
    if (!active.current) return;
    const request = ++latest.current;
    setState((current) => ({ ...current, loading: true }));
    try {
      const data = await invoke<T>(command);
      if (active.current && latest.current === request)
        setState({ data, loading: false, error: null });
    } catch (cause) {
      if (active.current && latest.current === request)
        setState((current) => ({
          ...current,
          loading: false,
          error: commandErrorText(cause),
        }));
    }
  }, [command]);
  useEffect(() => {
    active.current = true;
    void refresh();
    return () => {
      active.current = false;
      ++latest.current;
    };
  }, [refresh]);
  return { ...state, refresh };
}

export function useModelManagement() {
  const catalog = useModelRead<FullCatalog>("get_catalog");
  const installs = useModelRead<InstalledModel[]>("list_installed_models");
  const defaults = useModelRead<Record<string, string>>("get_default_models");
  const [pending, setPending] = useState<ModelAction | null>(null);
  const [actionError, setActionError] = useState<{
    action: ModelAction;
    detail: string;
  } | null>(null);
  const mutationPending = useRef(false);
  const active = useRef(false);
  useEffect(() => {
    active.current = true;
    return () => {
      active.current = false;
    };
  }, []);

  const refreshInstalls = useCallback(async () => {
    await Promise.all([installs.refresh(), defaults.refresh()]);
  }, [installs.refresh, defaults.refresh]);
  const ready = [catalog, installs, defaults].every(
    (resource) =>
      resource.data != null && !resource.loading && resource.error == null,
  );

  const run = useCallback(
    async (action: ModelAction) => {
      if (
        !active.current ||
        mutationPending.current ||
        (!ready && action.kind !== "cancel")
      )
        return;
      mutationPending.current = true;
      setPending(action);
      setActionError(null);
      try {
        if (action.kind === "remove")
          await invoke("delete_model", { modelId: action.modelId });
        else if (action.kind === "cancel")
          await invoke("cancel_model_download", { modelId: action.modelId });
        else
          await invoke("set_default_model", {
            task: action.task,
            modelId: action.modelId,
          });
        if (action.kind !== "cancel") notifySetupChanged();
      } catch (cause) {
        if (active.current) setActionError({ action, detail: commandErrorText(cause) });
      } finally {
        if (active.current) {
          await refreshInstalls();
          if (active.current) setPending(null);
        }
        mutationPending.current = false;
      }
    },
    [ready, refreshInstalls],
  );

  return {
    catalog,
    installs,
    defaults,
    pending,
    actionError,
    clearActionError: () => setActionError(null),
    refreshInstalls,
    run,
    ready,
  };
}
