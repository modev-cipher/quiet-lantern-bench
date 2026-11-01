import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";
import { openUrl } from "@tauri-apps/plugin-opener";
import { useEffect, useRef, useState } from "react";
import { useI18n, type MessageKey } from "./i18n";
import type { ProviderFamilyView } from "./providersModel";
import type {
  ProviderCatalog,
  ProviderDefinition,
  Recommendations,
} from "./providerSetupModel";

const ERROR_KEYS: Record<string, MessageKey> = {
  PROVIDER_INVALID_KEY: "providers.error.key",
  PROVIDER_PERMISSION_REQUIRED: "providers.error.permission",
  PROVIDER_RATE_LIMITED: "providers.error.quota",
  PROVIDER_UNREACHABLE: "providers.error.network",
  PROVIDER_NO_SUPPORTED_MODELS: "providers.error.models",
  PROVIDER_DISCOVERY_UNAVAILABLE: "providers.error.discovery",
  PROVIDER_CANCELLED: "providers.error.cancelled",
  PROVIDER_CATALOG_CHANGED: "providers.error.changed",
  PROVIDER_KEY_STORAGE_FAILED: "providers.error.storage",
};

export function ProviderSetup({
  definition,
  family,
  onMutated,
  onAdvanced,
}: {
  definition: ProviderDefinition;
  family: ProviderFamilyView;
  onMutated: () => void;
  onAdvanced: () => void;
}) {
  const { t } = useI18n();
  const [key, setKey] = useState("");
  const isConfigured =
    family.configuredKeys > family.counts.missing_bytes ||
    (definition.local &&
      family.connections.some(
        (connection) => connection.enabled && !connection.keyRequired,
      ));
  const [connected, setConnected] = useState(isConfigured);
  const [catalog, setCatalog] = useState<ProviderCatalog | null>(null);
  const [proposal, setProposal] = useState(false);
  const [customize, setCustomize] = useState(false);
  const [selected, setSelected] = useState<Recommendations>({
    script: null,
    narration: null,
    voice: null,
  });
  const [busy, setBusy] = useState(false);
  const [phase, setPhase] = useState<"discovering" | "saving" | "ready">(
    "discovering",
  );
  const [error, setError] = useState<string | null>(null);
  const generation = useRef(0);
  const operation = useRef<string | null>(null);
  const mounted = useRef(true);

  useEffect(() => {
    mounted.current = true;
    let active = true;
    const request = generation.current;
    const progress = listen<{
      operationId: string;
      phase: "discovering" | "saving" | "ready";
    }>("provider-setup-progress", ({ payload }) => {
      if (active && payload.operationId === operation.current)
        setPhase(payload.phase);
    });
    void invoke<ProviderCatalog | null>("provider_cached_catalog", {
      providerId: definition.id,
    })
      .then((cached) => {
        if (active && generation.current === request) setCatalog(cached);
      })
      .catch(() => undefined);
    return () => {
      active = false;
      void progress.then((unlisten) => unlisten()).catch(() => undefined);
      mounted.current = false;
      generation.current += 1;
      if (operation.current)
        void invoke("provider_cancel_setup", {
          operationId: operation.current,
        }).catch(() => undefined);
    };
  }, [definition.id]);

  useEffect(() => {
    setConnected(isConfigured);
  }, [isConfigured]);

  const run = async (action: () => Promise<void>) => {
    const request = ++generation.current;
    setBusy(true);
    setPhase("discovering");
    setError(null);
    try {
      await action();
    } catch (raw) {
      if (mounted.current && request === generation.current)
        setError(String(raw));
    } finally {
      if (mounted.current && request === generation.current) setBusy(false);
    }
  };

  const connect = async () => {
    const request = generation.current;
    operation.current ??= crypto.randomUUID();
    const fetched = await invoke<ProviderCatalog>("provider_connect", {
      providerId: definition.id,
      key: key.trim() || null,
      operationId: operation.current,
    });
    if (!mounted.current || request !== generation.current) return;
    setKey("");
    operation.current = null;
    setCatalog(fetched);
    setConnected(true);
    onMutated();
  };

  const refresh = async (continueListing = false) => {
    const request = generation.current;
    operation.current = crypto.randomUUID();
    const fetched = await invoke<ProviderCatalog>("provider_refresh_catalog", {
      providerId: definition.id,
      operationId: operation.current,
      continueListing,
    });
    if (!mounted.current || request !== generation.current) return;
    operation.current = null;
    setCatalog(fetched);
    setSelected(fetched.recommendations);
    setProposal(
      definition.preferredScript.length > 0 ||
        definition.preferredNarration.length > 0 ||
        definition.local,
    );
  };

  const apply = async () => {
    if (!catalog) return;
    await invoke("provider_apply_recommendations", {
      providerId: definition.id,
      revision: catalog.revision,
      selected,
    });
    if (!mounted.current) return;
    setCatalog((current) =>
      current ? { ...current, configured: selected } : current,
    );
    setProposal(false);
    setCustomize(false);
    onMutated();
  };

  const configurable =
    definition.preferredScript.length > 0 ||
    definition.preferredNarration.length > 0 ||
    definition.local;
  const needsSetup =
    family.configuredKeys > 0 &&
    family.connections.length === 0 &&
    definition.providerType !== null;
  const button =
    "rounded-xl border border-ink-700 px-4 py-2 text-label font-medium text-ink-100 transition-colors hover:border-ink-500 disabled:opacity-50";
  return (
    <section
      className="rounded-2xl border border-ink-800 bg-ink-900 p-5"
      aria-busy={busy}
    >
      {connected ? (
        <div data-testid="provider-connected" className="flex flex-col gap-4">
          <div>
            <h3 className="text-body font-semibold text-ink-100">
              {t("providers.connected")}
            </h3>
            <p className="mt-1 text-secondary text-ink-400">
              {t("providers.configuredHint")}
            </p>
            {catalog?.configured && (
              <div className="mt-3 flex flex-wrap gap-2 text-label text-ink-300">
                {catalog.configured.script && (
                  <span className="rounded-lg border border-ink-700 px-3 py-1">
                    {t(
                      catalog.models.find(
                        (model) => model.id === catalog.configured?.script,
                      )?.vision === "supported"
                        ? "providers.script"
                        : "providers.scriptOnly",
                    )}
                  </span>
                )}
                {catalog.configured.narration && catalog.configured.voice && (
                  <span className="rounded-lg border border-ink-700 px-3 py-1">
                    {t("providers.narration")}
                  </span>
                )}
              </div>
            )}
            {family.activeModels.length > 0 && (
              <p className="mt-2 text-label text-ink-300">
                <bdi dir="ltr">{family.activeModels.join(" · ")}</bdi>
              </p>
            )}
          </div>
          {needsSetup && (
            <p role="status" className="text-secondary text-ink-300">
              {t("providers.finishHint")}
            </p>
          )}
          {[...new Set(family.activeModels)].map((model) => {
            const date =
              catalog?.models.find((entry) => entry.id === model)?.retirement ??
              definition.retirements?.[model];
            return date ? (
              <p key={model} className="text-label text-wash-yellow-ink">
                {t("providers.retirementNotice", { model, date })}
              </p>
            ) : null;
          })}
          <div className="flex flex-wrap gap-2">
            {needsSetup && (
              <button
                type="button"
                data-testid="provider-finish-setup"
                className={button}
                disabled={busy}
                onClick={() => void run(connect)}
              >
                {t("providers.finishSetup")}
              </button>
            )}
            {configurable && (
              <button
                type="button"
                data-testid="provider-customize"
                className={button}
                disabled={busy}
                onClick={() => {
                  setCustomize((value) => !value);
                  if (catalog) {
                    setSelected(catalog.configured ?? catalog.recommendations);
                    setProposal(true);
                  } else void run(refresh);
                }}
              >
                {t("providers.customize")}
              </button>
            )}
            {!definition.local && (
              <button
                type="button"
                data-testid="provider-replace-key"
                className={button}
                disabled={busy}
                onClick={() => setConnected(false)}
              >
                {t("providers.replaceKey")}
              </button>
            )}
            <button
              type="button"
              data-testid="provider-manage-keys"
              className={button}
              onClick={onAdvanced}
            >
              {t("providers.manageKeys")}
            </button>
            <button
              type="button"
              data-testid="provider-refresh"
              className={button}
              disabled={busy}
              onClick={() => void run(refresh)}
            >
              {busy ? t("providers.checking") : t("providers.refresh")}
            </button>
          </div>
        </div>
      ) : (
        <form
          className="flex flex-col gap-4"
          onSubmit={(event) => {
            event.preventDefault();
            void run(connect);
          }}
        >
          <div>
            <h3 className="text-body font-semibold text-ink-100">
              {t(
                definition.local
                  ? "providers.connectLocal"
                  : "providers.connectTitle",
                { provider: definition.name },
              )}
            </h3>
            <p className="mt-1 text-secondary text-ink-400">
              {t(
                definition.local
                  ? "providers.localHint"
                  : "providers.setupHint",
              )}
            </p>
          </div>
          <div className="flex flex-wrap items-end gap-3">
            <label className="flex min-w-48 flex-1 flex-col gap-2 text-label text-ink-300">
              {t(
                definition.local
                  ? "providers.optionalToken"
                  : "providers.apiKey",
              )}
              <input
                data-testid="provider-setup-key"
                type="password"
                dir="ltr"
                value={key}
                autoComplete="off"
                spellCheck={false}
                disabled={busy}
                onChange={(event) => {
                  setKey(event.target.value);
                  operation.current = null;
                }}
                className="rounded-xl border border-ink-700 bg-ink-950 px-3 py-3 text-body text-ink-100 outline-none focus:border-ember-400"
              />
            </label>
            <button
              data-testid="provider-setup-connect"
              type="submit"
              disabled={busy || (!definition.local && !key.trim())}
              className="rounded-xl bg-ink-100 px-5 py-3 text-label font-semibold text-ink-950 disabled:opacity-50"
            >
              {t(busy ? "providers.checking" : "providers.connect")}
            </button>
          </div>
          {definition.keyUrl && (
            <button
              type="button"
              className="self-start text-label text-ember-400 underline underline-offset-2"
              onClick={() => void openUrl(definition.keyUrl!)}
            >
              {t("providers.getKey")}
            </button>
          )}
          <p className="text-label text-ink-500">
            {t("providers.destination", { host: definition.baseUrl ?? "" })}
          </p>
          <button
            type="button"
            data-testid="provider-advanced"
            className="self-start text-label text-ink-400 underline underline-offset-2"
            onClick={onAdvanced}
          >
            {t("providers.advanced")}
          </button>
        </form>
      )}
      {catalog && !catalog.voicesComplete && (
        <p className="mt-3 text-label text-wash-yellow-ink">
          {t("providers.voicesPartial")}
        </p>
      )}
      {catalog && !catalog.complete && (
        <p className="mt-3 text-label text-wash-yellow-ink">
          {t("providers.partial")}{" "}
          {catalog.nextCursor && (
            <button
              type="button"
              data-testid="provider-load-more"
              disabled={busy}
              onClick={() => void run(() => refresh(true))}
              className="underline"
            >
              {t("providers.loadMore")}
            </button>
          )}
        </p>
      )}
      {catalog && proposal && (
        <div
          className="mt-4 flex flex-col gap-3 rounded-xl border border-ink-700 p-4"
          data-testid="provider-recommendations"
        >
          <p className="text-secondary text-ink-300">
            {t("providers.proposal")}
          </p>
          {(["script", "narration"] as const).map(
            (role) =>
              selected[role] !== null && (
                <label
                  key={role}
                  className="flex flex-col gap-2 text-label text-ink-400"
                >
                  {t(
                    role === "script"
                      ? "providers.script"
                      : "providers.narration",
                  )}
                  {customize ? (
                    <input
                      dir="ltr"
                      data-testid={`provider-model-${role}`}
                      value={selected[role] ?? ""}
                      list={`provider-models-${role}`}
                      onChange={(event) =>
                        setSelected((value) => ({
                          ...value,
                          [role]: event.target.value,
                        }))
                      }
                      className="rounded-lg border border-ink-700 bg-ink-950 p-2 font-mono text-ink-100"
                    />
                  ) : (
                    <bdi dir="ltr" className="text-ink-100">
                      {selected[role]}
                    </bdi>
                  )}
                  <datalist id={`provider-models-${role}`}>
                    {catalog.models
                      .filter(
                        (model) =>
                          model[role] !== "unsupported" &&
                          model.id
                            .toLowerCase()
                            .includes((selected[role] ?? "").toLowerCase()),
                      )
                      .slice(0, 200)
                      .map((model) => (
                        <option key={model.id} value={model.id}>
                          {model.name}
                          {model[role] === "unknown"
                            ? ` · ${t("providers.unknownCapability")}`
                            : ""}
                          {model.preview ? ` · ${t("providers.preview")}` : ""}
                          {!model.active
                            ? ` · ${t("providers.unavailable")}`
                            : ""}
                        </option>
                      ))}
                  </datalist>
                </label>
              ),
          )}
          {customize && catalog.voices.length > 0 && (
            <label className="flex flex-col gap-2 text-label text-ink-400">
              {t("providers.voice")}
              <select
                value={selected.voice ?? ""}
                onChange={(event) =>
                  setSelected((value) => ({
                    ...value,
                    voice: event.target.value,
                  }))
                }
                className="rounded-lg border border-ink-700 bg-ink-950 p-2 text-ink-100"
              >
                {catalog.voices.map((voice) => (
                  <option key={voice.id} value={voice.id}>
                    {voice.name}
                  </option>
                ))}
              </select>
            </label>
          )}
          <button
            type="button"
            data-testid="provider-apply"
            className={button}
            disabled={busy}
            onClick={() => void run(apply)}
          >
            {t("providers.apply")}
          </button>
        </div>
      )}
      {busy && (
        <div
          role="status"
          aria-live="polite"
          className="mt-3 flex items-center gap-3 text-label text-ink-300"
        >
          <span>{t(`providers.phase.${phase}`)}</span>
          {operation.current && (
            <button
              type="button"
              data-testid="provider-setup-cancel"
              className="underline"
              onClick={() => {
                generation.current += 1;
                const id = operation.current;
                operation.current = null;
                setBusy(false);
                if (id)
                  void invoke("provider_cancel_setup", {
                    operationId: id,
                  }).catch(() => undefined);
              }}
            >
              {t("providers.copy.2")}
            </button>
          )}
        </div>
      )}
      {error && (
        <p
          role="alert"
          data-testid="provider-setup-error"
          className="mt-3 text-secondary text-wash-red-ink"
        >
          {t(ERROR_KEYS[error] ?? "providers.error.generic")}
        </p>
      )}
    </section>
  );
}
