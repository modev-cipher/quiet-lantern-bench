import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";
import { openUrl } from "@tauri-apps/plugin-opener";
import { useCallback, useEffect, useRef, useState } from "react";

import { ConnectionPresetMark, FamilyMark } from "./providerGlyphs";
import { CustomDropdown } from "./CustomDropdown";
import { ProviderSetup } from "./ProviderSetup";
import {
  projectProviders,
  type ProviderDefinition,
} from "./providerSetupModel";
import {
  GO_VIEW_EVENT,
  consumePendingProvider,
  notifyVoiceKeysChanged,
} from "./views";
import { useI18n, t as providerText } from "./i18n";
import {
  activeModelsLine,
  cardLabelText,
  cardPoolLine,
  countsSummary,
  modelsCacheAge,
  parseDeleteImpact,
  poolLabelText,
  poolTestKey,
  slotHealthText,
  visibleSlots,
  type CachedModels,
  type CastConsentRow,
  type PoolSlotView,
  type PoolTestProgress,
  type PoolView,
  type ProviderConnectionView,
  type ProviderFamilyView,
} from "./providersModel";

// Settings → Providers (S-5, Hermes IA ruling): the top level is one calm
// compact card per provider family — provider identification, capability
// facts, active models, and ONE aggregated pool line. Raw key rows never
// render here at any pool size. Selecting a card opens the dedicated
// full-page sub-view (breadcrumb Back) that owns connections, the key
// pool, consent revocation, retention links, and vault state.
//
// Provider marks: bundled community glyphs rendered inline via
// providerGlyphs.tsx (sources, versions, licenses, and trademark note in
// assets/providers/manifest.json), aria-hidden beside the provider's
// textual name. Families without a lawful glyph use text identity
// (asset-law rule 8, fail-closed).

const TEST_PROGRESS_EVENT = "provider-pool-test-progress";

/// The one custody family whose endpoints are user-supplied; every other
/// family's URL is adapter-owned, so its key adds send NO base URL.
const CUSTOM_ENDPOINT_FAMILY = "openai_compat";

function capabilityLabel(capability: string): string {
  const keys: Record<string, import("./i18n").MessageKey> = {
    Scripting: "providers.scriptOnly",
    script_llm: "providers.scriptOnly",
    "Audio voices": "providers.narration",
    tts: "providers.narration",
    "Vision & charts": "providers.figures",
    vision_mllm: "providers.figures",
    "Voice Tutor": "providers.tutor",
    "Voice Tutor speech": "providers.speech",
    "Voice Tutor speech-to-text": "providers.speechToText",
    "Book agent": "providers.bookAgent",
    agent_live: "providers.bookAgent",
  };
  return keys[capability] ? providerText(keys[capability]) : capability;
}

function ProviderMark({ family }: { family: ProviderFamilyView }) {
  return (
    <FamilyMark
      familyId={family.id}
      displayName={
        family.id === "custom"
          ? providerText("providers.customName")
          : family.displayName
      }
    />
  );
}

function ConnectionMark({
  connection,
}: {
  connection: ProviderConnectionView;
}) {
  return (
    <ConnectionPresetMark baseUrl={connection.baseUrl} name={connection.name} />
  );
}

function CardStatusDot({ family }: { family: ProviderFamilyView }) {
  const kind = family.card.kind;
  const tone =
    kind === "healthy"
      ? "bg-wash-green-ink"
      : kind === "not_configured" || kind === "disabled"
        ? "bg-ink-600"
        : kind === "degraded" || kind === "degraded_checking"
          ? "bg-wash-yellow-ink"
          : kind === "checking_all" || kind === "checking_some"
            ? "bg-ink-400"
            : "bg-wash-red-ink";
  return (
    <span
      aria-hidden="true"
      className={`h-2 w-2 shrink-0 rounded-full ${tone}`}
    />
  );
}

function ProviderCard({
  family,
  onOpen,
}: {
  family: ProviderFamilyView;
  onOpen: () => void;
}) {
  const { dir } = useI18n();
  const models = activeModelsLine(family.activeModels);
  return (
    <button
      type="button"
      data-testid={`provider-card-${family.id}`}
      onClick={onOpen}
      className="flex w-full items-center gap-4 rounded-2xl border border-ink-800 bg-ink-900 p-5 text-start transition-colors hover:border-ink-700"
    >
      <ProviderMark family={family} />
      <span className="min-w-0 flex-1">
        <span className="flex items-baseline gap-2.5">
          <span className="truncate font-serif text-title font-semibold text-ink-50">
            {family.id === "custom"
              ? providerText("providers.customName")
              : family.displayName}
          </span>
          <span className="truncate text-label text-ink-500">
            {family.capabilities.map(capabilityLabel).join(" · ")}
          </span>
        </span>
        <span
          data-testid={`provider-pool-line-${family.id}`}
          className="mt-1 flex items-center gap-2 text-secondary text-ink-400"
        >
          <CardStatusDot family={family} />
          {cardPoolLine(family)}
        </span>
        <span className="sr-only">{countsSummary(family.counts)}</span>
        {models != null && (
          <span className="mt-0.5 block truncate font-mono text-label text-ink-500">
            {models}
          </span>
        )}
        {family.disabledConnections > 0 && (
          <span className="mt-0.5 block text-label text-ink-500">
            {providerText("providers.disabledConnections", {
              count: family.disabledConnections,
            })}
          </span>
        )}
      </span>
      <span aria-hidden="true" className="text-ink-500">
        {dir === "rtl" ? "‹" : "›"}
      </span>
    </button>
  );
}

function SlotRow({
  slot,
  onToggleDisabled,
  onRefresh,
  onRemove,
}: {
  slot: PoolSlotView;
  onToggleDisabled: () => void;
  onRefresh: () => void;
  onRemove: () => void;
}) {
  return (
    <li
      data-testid={`pool-slot-${slot.account}`}
      className="flex items-center gap-3 border-b border-ink-800/60 py-2.5 last:border-b-0"
    >
      <span className="w-10 shrink-0 font-mono text-label text-ink-500">
        #{slot.slot}
      </span>
      <span className="min-w-0 flex-1 truncate text-secondary text-ink-200">
        {slot.label ?? providerText("providers.copy.39")}
        {slot.last4 != null && (
          <span className="ms-2 font-mono text-label text-ink-500">
            ····{slot.last4}
          </span>
        )}
      </span>
      <span
        data-testid={`pool-slot-state-${slot.account}`}
        className={`shrink-0 text-label ${
          slot.health === "working"
            ? "text-wash-green-ink"
            : slot.health === "disabled"
              ? "text-ink-500"
              : slot.health === "unverified" || slot.health === "checking"
                ? "text-ink-400"
                : "text-wash-yellow-ink"
        }`}
      >
        {slotHealthText(slot.health)}
        {slot.health === "rate_limited" && slot.retryAfterUnixMs != null && (
          <>
            {" "}
            {providerText("providers.retryShort")}{" "}
            {new Date(slot.retryAfterUnixMs).toLocaleTimeString([], {
              hour: "2-digit",
              minute: "2-digit",
            })}
          </>
        )}
      </span>
      <button
        type="button"
        data-testid={`pool-slot-toggle-${slot.account}`}
        onClick={onToggleDisabled}
        className="shrink-0 text-label font-medium text-ink-400 transition-colors hover:text-ink-200"
      >
        {slot.health === "disabled"
          ? providerText("providers.copy.40")
          : providerText("providers.copy.41")}
      </button>
      <button
        type="button"
        data-testid={`pool-slot-refresh-${slot.account}`}
        onClick={onRefresh}
        className="shrink-0 text-label font-medium text-ink-400 transition-colors hover:text-ink-200"
      >
        {providerText("providers.copy.0")}
      </button>
      <button
        type="button"
        data-testid={`pool-slot-remove-${slot.account}`}
        onClick={onRemove}
        className="shrink-0 text-label font-medium text-wash-red-ink/80 transition-colors hover:text-wash-red-ink"
      >
        {providerText("providers.copy.1")}
      </button>
    </li>
  );
}

function PoolSection({
  family,
  pool,
  test,
  onMutated,
  onError,
}: {
  family: ProviderFamilyView;
  pool: PoolView;
  test: PoolTestProgress | null;
  onMutated: () => void;
  onError: (message: string) => void;
}) {
  const [showAll, setShowAll] = useState(false);
  const [addKey, setAddKey] = useState("");
  const [addLabel, setAddLabel] = useState("");
  const [bulkText, setBulkText] = useState("");
  const [bulkOpen, setBulkOpen] = useState(false);
  const [receipt, setReceipt] = useState<string | null>(null);
  const [confirmRemove, setConfirmRemove] = useState<string | null>(null);
  const [pendingConsent, setPendingConsent] = useState<{
    canonicalUrl: string;
    action: "add" | "bulk";
  } | null>(null);
  const { visible, hidden } = visibleSlots(pool.slots, showAll);
  // Checking is a backend fact (the overview projects the manager's pool
  // test registry); the event stream only makes the counter live.
  const testing =
    pool.label.kind === "checking" || (test != null && !test.done);
  // Fixed families own their URLs adapter-side: sending one is refused
  // (BaseUrlFixed). Only the custom family passes the pool URL, and only
  // after the URL-consent gate below.
  const custom = (family.custodyId ?? family.id) === CUSTOM_ENDPOINT_FAMILY;
  const keyBaseUrl = custom ? pool.baseUrl : null;

  const clearBuffers = () => {
    setAddKey("");
    setAddLabel("");
    setBulkText("");
  };
  // Input buffers never survive navigation away (sub-page rule 2).
  useEffect(() => clearBuffers, []);

  const performAdd = async () => {
    try {
      await invoke("add_voice_key", {
        provider: family.custodyId ?? family.id,
        baseUrl: keyBaseUrl,
        key: addKey,
        label: addLabel.trim() === "" ? null : addLabel.trim(),
      });
      clearBuffers();
      setReceipt(providerText("providers.copy.43"));
      onMutated();
    } catch (raw) {
      setReceipt(String(raw));
    }
  };

  const performBulk = async () => {
    try {
      const report = await invoke<{
        imported: unknown[];
        rejected: unknown[];
        duplicates: unknown[];
      }>("import_voice_keys_text", {
        provider: family.custodyId ?? family.id,
        baseUrl: keyBaseUrl,
        text: bulkText,
      });
      clearBuffers();
      setBulkOpen(false);
      setReceipt(
        providerText("providers.importReceipt", {
          imported: report.imported.length,
          rejected: report.rejected.length,
          count: report.duplicates.length,
        }),
      );
      onMutated();
    } catch (raw) {
      setReceipt(String(raw));
    }
  };

  /// Custom endpoints need recorded URL consent before key bytes are
  /// stored for them (charter consent law). Fixed providers skip this —
  /// their endpoints are adapter-owned and pre-lawful.
  const withUrlConsent = async (action: "add" | "bulk") => {
    if (!custom) {
      return action === "add" ? performAdd() : performBulk();
    }
    try {
      const check = await invoke<{ canonicalUrl: string; consented: boolean }>(
        "check_voice_url",
        { url: pool.baseUrl },
      );
      if (!check.consented) {
        setPendingConsent({ canonicalUrl: check.canonicalUrl, action });
        return;
      }
      return action === "add" ? performAdd() : performBulk();
    } catch (raw) {
      setReceipt(String(raw));
    }
  };

  const submitKey = async () => {
    if (addKey.trim().length === 0) return;
    await withUrlConsent("add");
  };

  const submitBulk = async () => {
    if (bulkText.trim().length === 0) return;
    await withUrlConsent("bulk");
  };

  return (
    <section
      data-testid={`pool-${family.id}-${pool.baseUrl}`}
      className="rounded-2xl border border-ink-800 bg-ink-900 p-5"
    >
      <div className="flex flex-wrap items-center justify-between gap-3">
        <div className="min-w-0">
          <h3 className="truncate font-mono text-secondary text-ink-200">
            {pool.baseUrl}
          </h3>
          <p
            data-testid={`pool-label-${pool.baseUrl}`}
            className="mt-0.5 text-secondary text-ink-400"
          >
            {test != null && !test.done
              ? providerText("providers.progress", {
                  completed: test.completed,
                  total: test.total,
                })
              : test != null && test.cancelled
                ? providerText("providers.stopped", {
                    completed: test.completed,
                    total: test.total,
                  })
                : poolLabelText(pool.label)}
            {" · "}
            {providerText("providers.keysConfigured", {
              count: pool.configured,
            })}
            {pool.operational !== pool.configured &&
              providerText("providers.enabledCount", {
                count: pool.operational,
              })}
            {pool.voiceTutorBuiltIn && providerText("providers.copy.44")}
          </p>
          <p className="sr-only">{countsSummary(pool.counts)}</p>
        </div>
        {pool.configured > 0 && (
          <div className="flex items-center gap-2">
            <button
              type="button"
              data-testid={`pool-test-${pool.baseUrl}`}
              disabled={testing}
              onClick={() =>
                // The terminal progress event triggers the ONE projection
                // reload; resolving the command must not add a second.
                void invoke("provider_pool_test", {
                  provider: family.custodyId ?? family.id,
                  baseUrl: pool.baseUrl,
                }).catch((raw) => onError(String(raw)))
              }
              className="rounded-lg border border-ink-700 px-3 py-1.5 text-label font-medium text-ink-200 transition-colors hover:border-ink-600 disabled:opacity-60"
            >
              {testing
                ? providerText("providers.copy.45")
                : providerText("providers.copy.46")}
            </button>
            {testing && (
              <button
                type="button"
                data-testid={`pool-test-cancel-${pool.baseUrl}`}
                onClick={() =>
                  void invoke("provider_pool_test_cancel", {
                    provider: family.custodyId ?? family.id,
                    baseUrl: pool.baseUrl,
                  }).catch((raw) => onError(String(raw)))
                }
                className="rounded-lg border border-ink-700 px-3 py-1.5 text-label font-medium text-ink-400 transition-colors hover:border-ink-600 hover:text-ink-200"
              >
                {providerText("providers.copy.2")}
              </button>
            )}
          </div>
        )}
      </div>

      {pendingConsent != null && (
        <div
          data-testid="pool-url-consent"
          className="mt-3 flex flex-wrap items-center gap-3 rounded-lg border border-wash-yellow-ink/30 bg-wash-yellow px-3 py-2 text-secondary text-wash-yellow-ink"
        >
          {providerText("providers.copy.60")}{" "}
          <span className="font-mono text-label">
            {pendingConsent.canonicalUrl}
          </span>
          {providerText("providers.copy.61")}
          <button
            type="button"
            data-testid="pool-url-consent-allow"
            onClick={() => {
              const pending = pendingConsent;
              setPendingConsent(null);
              void invoke("record_voice_url_consent", {
                url: pending.canonicalUrl,
              })
                .then(() =>
                  pending.action === "add" ? performAdd() : performBulk(),
                )
                .catch((raw) => onError(String(raw)));
            }}
            className="font-semibold underline underline-offset-2"
          >
            {providerText("providers.copy.3")}
          </button>
          <button
            type="button"
            data-testid="pool-url-consent-decline"
            onClick={() => setPendingConsent(null)}
            className="font-medium"
          >
            {providerText("providers.copy.4")}
          </button>
        </div>
      )}

      {pool.slots.length > 0 && (
        <ul data-testid={`pool-slots-${pool.baseUrl}`} className="mt-3">
          {visible.map((slot) => (
            <SlotRow
              key={slot.account}
              slot={slot}
              onToggleDisabled={() =>
                void invoke("provider_slot_set_disabled", {
                  account: slot.account,
                  disabled: slot.health !== "disabled",
                })
                  .then(onMutated)
                  .catch((raw) => onError(String(raw)))
              }
              onRefresh={() =>
                void invoke("refresh_voice_key", {
                  baseUrl: slot.account,
                })
                  .then(onMutated)
                  .catch((raw) => onError(String(raw)))
              }
              onRemove={() => setConfirmRemove(slot.account)}
            />
          ))}
        </ul>
      )}
      {hidden > 0 && (
        <button
          type="button"
          data-testid={`pool-show-all-${pool.baseUrl}`}
          onClick={() => setShowAll(true)}
          className="mt-2 text-label font-medium text-ember-400 transition-colors hover:text-ember-300"
        >
          {providerText("providers.showAllKeys", {
            count: pool.slots.length,
            hidden,
          })}
        </button>
      )}

      {confirmRemove != null && (
        <div
          data-testid="pool-remove-confirm"
          className="mt-3 flex items-center gap-3 rounded-lg border border-wash-red-ink/30 bg-wash-red px-3 py-2 text-secondary text-wash-red-ink"
        >
          {providerText("providers.copy.59")}
          <button
            type="button"
            data-testid="pool-remove-confirm-yes"
            onClick={() => {
              const account = confirmRemove;
              setConfirmRemove(null);
              void invoke("remove_voice_key", { baseUrl: account })
                .then(onMutated)
                .catch((raw) => onError(String(raw)));
            }}
            className="font-semibold underline underline-offset-2"
          >
            {providerText("providers.copy.1")}
          </button>
          <button
            type="button"
            onClick={() => setConfirmRemove(null)}
            className="font-medium"
          >
            {providerText("providers.copy.5")}
          </button>
        </div>
      )}

      <div className="mt-4 flex flex-wrap items-end gap-2">
        <label className="flex min-w-0 flex-1 flex-col gap-1 text-label text-ink-500">
          {providerText("providers.copy.6")}
          <input
            type="password"
            data-testid={`pool-add-key-${pool.baseUrl}`}
            value={addKey}
            onChange={(event) => setAddKey(event.target.value)}
            autoComplete="off"
            className="rounded-lg border border-ink-800 bg-ink-950 px-2.5 py-1.5 font-mono text-secondary text-ink-200"
          />
        </label>
        <label className="flex w-36 flex-col gap-1 text-label text-ink-500">
          {providerText("providers.copy.7")}
          <input
            type="text"
            data-testid={`pool-add-label-${pool.baseUrl}`}
            value={addLabel}
            onChange={(event) => setAddLabel(event.target.value)}
            className="rounded-lg border border-ink-800 bg-ink-950 px-2.5 py-1.5 text-secondary text-ink-200"
          />
        </label>
        <button
          type="button"
          data-testid={`pool-add-submit-${pool.baseUrl}`}
          onClick={() => void submitKey()}
          className="rounded-lg bg-ink-100 px-3 py-1.5 text-label font-semibold text-ink-950 transition-colors hover:bg-ink-50"
        >
          {providerText("providers.copy.8")}
        </button>
        <button
          type="button"
          data-testid={`pool-bulk-open-${pool.baseUrl}`}
          onClick={() => setBulkOpen((open) => !open)}
          className="rounded-lg border border-ink-700 px-3 py-1.5 text-label font-medium text-ink-200 transition-colors hover:border-ink-600"
        >
          {providerText("providers.copy.9")}
        </button>
      </div>
      {bulkOpen && (
        <div className="mt-2 flex flex-col gap-2">
          <textarea
            data-testid={`pool-bulk-text-${pool.baseUrl}`}
            value={bulkText}
            onChange={(event) => setBulkText(event.target.value)}
            rows={4}
            placeholder={providerText("providers.copy.47")}
            className="rounded-lg border border-ink-800 bg-ink-950 px-2.5 py-1.5 font-mono text-label text-ink-200"
          />
          <button
            type="button"
            data-testid={`pool-bulk-submit-${pool.baseUrl}`}
            onClick={() => void submitBulk()}
            className="self-start rounded-lg bg-ink-100 px-3 py-1.5 text-label font-semibold text-ink-950 transition-colors hover:bg-ink-50"
          >
            {providerText("providers.copy.10")}
          </button>
        </div>
      )}
      {receipt != null && (
        <p
          data-testid={`pool-receipt-${pool.baseUrl}`}
          className="mt-2 text-label text-ink-400"
        >
          {receipt}
        </p>
      )}
    </section>
  );
}

function kindLabel(kind: string): string {
  return providerText(
    kind === "openai_tts" || kind === "cartesia_tts"
      ? "providers.narration"
      : "providers.scriptOnly",
  );
}

function consentLabel(value: string): string {
  const keys: Record<string, import("./i18n").MessageKey> = {
    script: "providers.scriptOnly",
    tts: "providers.narration",
    vision: "providers.figures",
    agent: "providers.bookAgent",
    source_text: "providers.data.source",
    script_text: "providers.data.script",
    figure_crops: "providers.data.figures",
    voice_audio: "providers.data.audio",
    voice_transcript: "providers.data.transcript",
    reading_position: "providers.data.position",
    page_images: "providers.data.pages",
  };
  return keys[value] ? providerText(keys[value]) : value.split("_").join(" ");
}

/// Delete-impact prompt state: `blocked` means the impact could not be
/// verified and deletion is refused (fail-CLOSED, checkpoint-3 blocker 4).
type ImpactState =
  { kind: "known"; summary: string } | { kind: "blocked"; reason: string };

function ConnectionRow({
  connection,
  onMutated,
}: {
  connection: ProviderConnectionView;
  onMutated: () => void;
}) {
  const [renaming, setRenaming] = useState(false);
  const [name, setName] = useState(connection.name);
  const [impact, setImpact] = useState<ImpactState | null>(null);
  const [error, setError] = useState<string | null>(null);

  const update = async (fields: {
    name?: string;
    defaultModel?: string;
    enabled?: boolean;
  }) => {
    try {
      // The full record round-trips: unchanged fields are re-sent as they
      // stand, so validation sees the same lawful shape it stored.
      await invoke("provider_connection_update", {
        id: connection.id,
        name: fields.name ?? connection.name,
        baseUrl: connection.baseUrl,
        credentialNotRequired: !connection.keyRequired,
        defaultModel: fields.defaultModel ?? connection.defaultModel,
        enabled: fields.enabled ?? connection.enabled,
      });
      setError(null);
      onMutated();
    } catch (raw) {
      setError(String(raw));
    }
  };

  const askDelete = async () => {
    let report: ReturnType<typeof parseDeleteImpact>;
    try {
      report = parseDeleteImpact(
        await invoke("provider_connection_delete_impact", {
          id: connection.id,
        }),
      );
    } catch (raw) {
      setImpact({ kind: "blocked", reason: String(raw) });
      return;
    }
    if (report == null) {
      setImpact({
        kind: "blocked",
        reason: providerText("providers.copy.65"),
      });
      return;
    }
    const parts: string[] = [];
    if (report.referencingJobs.length > 0)
      parts.push(
        providerText("providers.deleteJobs", {
          count: report.referencingJobs.length,
        }),
      );
    if (report.referencingVoiceProfiles.length > 0)
      parts.push(
        providerText("providers.deleteVoices", {
          count: report.referencingVoiceProfiles.length,
          voices: report.referencingVoiceProfiles.join(", "),
        }),
      );
    parts.push(providerText("providers.copy.64"));
    setImpact({ kind: "known", summary: parts.join(" · ") });
  };

  return (
    <li
      data-testid={`connection-row-${connection.id}`}
      className="flex flex-wrap items-center gap-3 border-b border-ink-800/60 py-3 last:border-b-0"
    >
      {renaming ? (
        <input
          data-testid={`connection-rename-${connection.id}`}
          value={name}
          onChange={(event) => setName(event.target.value)}
          onKeyDown={(event) => {
            if (event.key === "Enter") {
              setRenaming(false);
              void update({ name });
            }
            if (event.key === "Escape") {
              setName(connection.name);
              setRenaming(false);
            }
          }}
          className="rounded-lg border border-ink-700 bg-ink-950 px-2 py-1 text-secondary text-ink-100"
        />
      ) : (
        <span className="flex min-w-0 flex-1 items-center gap-2.5 truncate">
          {connection.baseUrl != null && (
            <ConnectionMark connection={connection} />
          )}
          <span className="min-w-0 truncate">
            <span className="text-body font-medium text-ink-100">
              {connection.name}
            </span>
            <span className="ms-2 text-label text-ink-500">
              {kindLabel(connection.providerType)}
              {connection.hostname != null && ` · ${connection.hostname}`}
              {" · "}
              <span className="font-mono">{connection.defaultModel}</span>
            </span>
            {!connection.enabled && (
              <span className="ms-2 rounded bg-ink-800 px-1.5 py-0.5 text-label text-ink-400">
                {providerText("providers.copy.42")}
              </span>
            )}
          </span>
        </span>
      )}
      <button
        type="button"
        data-testid={`connection-rename-btn-${connection.id}`}
        onClick={() => setRenaming((r) => !r)}
        className="text-label font-medium text-ink-400 transition-colors hover:text-ink-200"
      >
        {providerText("providers.copy.11")}
      </button>
      <button
        type="button"
        data-testid={`connection-toggle-${connection.id}`}
        onClick={() => void update({ enabled: !connection.enabled })}
        className="text-label font-medium text-ink-400 transition-colors hover:text-ink-200"
      >
        {connection.enabled
          ? providerText("providers.copy.41")
          : providerText("providers.copy.40")}
      </button>
      <button
        type="button"
        data-testid={`connection-delete-${connection.id}`}
        onClick={() => void askDelete()}
        className="text-label font-medium text-wash-red-ink/80 transition-colors hover:text-wash-red-ink"
      >
        {providerText("providers.copy.12")}
      </button>
      {impact != null && (
        <span
          data-testid={`connection-impact-${connection.id}`}
          className="flex w-full flex-wrap items-center gap-3 rounded-lg border border-wash-red-ink/30 bg-wash-red px-3 py-2 text-label text-wash-red-ink"
        >
          {impact.kind === "blocked" ? (
            <>
              {providerText("providers.copy.62")}
              {impact.reason}
              {providerText("providers.copy.63")}
              <button
                type="button"
                onClick={() => setImpact(null)}
                className="font-medium"
              >
                {providerText("providers.copy.13")}
              </button>
            </>
          ) : (
            <>
              {impact.summary}
              <button
                type="button"
                data-testid={`connection-delete-confirm-${connection.id}`}
                onClick={() =>
                  void invoke("provider_connection_delete", {
                    id: connection.id,
                  })
                    .then(onMutated)
                    .catch((raw) =>
                      setError(
                        providerText("providers.deleteFailed", {
                          reason: String(raw),
                        }),
                      ),
                    )
                }
                className="font-semibold underline underline-offset-2"
              >
                {providerText("providers.copy.14")}
              </button>
              <button
                type="button"
                onClick={() => setImpact(null)}
                className="font-medium"
              >
                {providerText("providers.copy.5")}
              </button>
            </>
          )}
        </span>
      )}
      {error != null && (
        <span
          data-testid={`connection-error-${connection.id}`}
          className="w-full text-label text-wash-red-ink"
        >
          {error}
        </span>
      )}
      <ModelsSection connection={connection} onApplyDefault={update} />
      {connection.capabilities.includes("tts") && (
        <CloudVoiceForm connection={connection} />
      )}
    </li>
  );
}

/// Owner feedback F1 (2026-08-29): the one searchable model list, shared
/// by the connection editor and the add-connection form. Picking a row is
/// a suggestion applied to the model field — the free-text override
/// stands beside it (T5).
function ModelSearchList({
  models,
  suggested,
  current,
  searchTestId,
  pickTestId,
  onPick,
}: {
  models: string[];
  suggested: string | null;
  current: string | null;
  searchTestId: string;
  pickTestId: (model: string) => string;
  onPick: (model: string) => void;
}) {
  const [search, setSearch] = useState("");
  const needle = search.trim().toLowerCase();
  const shown = models.filter(
    (model) => needle === "" || model.toLowerCase().includes(needle),
  );
  return (
    <>
      <input
        data-testid={searchTestId}
        value={search}
        onChange={(event) => setSearch(event.target.value)}
        placeholder={providerText("providers.copy.48")}
        className="mt-2 w-full rounded-lg border border-ink-800 bg-ink-900 px-2.5 py-1.5 font-mono text-label text-ink-200"
      />
      <ul className="mt-1 max-h-48 overflow-y-auto">
        {shown.slice(0, 50).map((model) => (
          <li
            key={model}
            className="flex items-center gap-3 border-b border-ink-800/60 py-1.5 last:border-b-0"
          >
            <span className="min-w-0 flex-1 truncate font-mono text-label text-ink-300">
              {model}
              {suggested === model && (
                <span className="ms-2 text-ink-500">
                  {providerText("providers.copy.15")}
                </span>
              )}
            </span>
            {current === model ? (
              <span className="text-label text-ink-500">
                {providerText("providers.copy.16")}
              </span>
            ) : (
              <button
                type="button"
                data-testid={pickTestId(model)}
                onClick={() => onPick(model)}
                className="text-label font-medium text-ember-400 transition-colors hover:text-ember-300"
              >
                {providerText("providers.copy.17")}
              </button>
            )}
          </li>
        ))}
        {shown.length === 0 && (
          <li className="py-1.5 text-label text-ink-500">
            {providerText("providers.copy.18")}
          </li>
        )}
      </ul>
      {shown.length > 50 && (
        <p className="mt-1 text-label text-ink-500">
          {providerText("providers.showingModels", { count: shown.length })}
        </p>
      )}
    </>
  );
}

/// S-5 blocker 7, reshaped by owner feedback F1 (2026-08-29): opening the
/// model panel on a keyed (or lawful keyless) connection fetches the live
/// list when no cache exists — the dropdown appears without a second
/// click. The cache still renders with its honest age fact and a manual
/// Refresh; a listing failure is an inline error with the free-text
/// override still standing (never a dead dropdown).
function ModelsSection({
  connection,
  onApplyDefault,
}: {
  connection: ProviderConnectionView;
  onApplyDefault: (fields: { defaultModel: string }) => Promise<void>;
}) {
  const [open, setOpen] = useState(false);
  const [cached, setCached] = useState<CachedModels | null>(null);
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [custom, setCustom] = useState("");

  const fetchNow = async () => {
    setBusy(true);
    setError(null);
    try {
      await invoke("provider_fetch_models", { connectionId: connection.id });
      setCached(
        await invoke<CachedModels | null>("provider_cached_models", {
          connectionId: connection.id,
        }),
      );
    } catch (raw) {
      setError(String(raw));
    } finally {
      setBusy(false);
    }
  };

  // Opening reads the local cache first; with no cache and a key present
  // (or a lawful keyless connection), the live list is fetched right away
  // (F1) — otherwise the panel says honestly why there is nothing to show.
  const canFetch = connection.keyPresent || !connection.keyRequired;
  const openSection = async () => {
    setOpen(true);
    let hit: CachedModels | null = null;
    try {
      hit = await invoke<CachedModels | null>("provider_cached_models", {
        connectionId: connection.id,
      });
    } catch {
      hit = null;
    }
    setCached(hit);
    if (hit == null && canFetch) {
      await fetchNow();
    }
  };

  if (!open) {
    return (
      <button
        type="button"
        data-testid={`connection-models-${connection.id}`}
        onClick={() => void openSection()}
        className="text-label font-medium text-ink-400 transition-colors hover:text-ink-200"
      >
        {providerText("providers.copy.19")}
      </button>
    );
  }
  return (
    <div
      data-testid={`connection-models-panel-${connection.id}`}
      className="mt-2 w-full rounded-lg border border-ink-800 bg-ink-950 p-3"
    >
      <div className="flex flex-wrap items-center gap-3">
        <span className="text-label font-medium text-ink-300">
          {providerText("providers.copy.20")}
        </span>
        <button
          type="button"
          data-testid={`connection-models-fetch-${connection.id}`}
          disabled={busy}
          onClick={() => void fetchNow()}
          className="rounded-lg border border-ink-700 px-2.5 py-1 text-label font-medium text-ink-200 transition-colors hover:border-ink-600 disabled:opacity-60"
        >
          {busy
            ? providerText("providers.copy.49")
            : cached != null
              ? providerText("providers.copy.50")
              : providerText("providers.copy.51")}
        </button>
        {cached != null && (
          <span
            data-testid={`connection-models-age-${connection.id}`}
            className="text-label text-ink-500"
          >
            {modelsCacheAge(cached.fetchedUnix, Math.floor(Date.now() / 1000))}
          </span>
        )}
        <button
          type="button"
          onClick={() => setOpen(false)}
          className="ms-auto text-label font-medium text-ink-500 transition-colors hover:text-ink-300"
        >
          {providerText("providers.copy.13")}
        </button>
      </div>
      {error != null && (
        <p
          data-testid={`connection-models-error-${connection.id}`}
          className="mt-2 text-label text-wash-red-ink"
        >
          {error}
        </p>
      )}
      {cached == null && error == null && !busy && (
        <p className="mt-2 text-label text-ink-500">
          {canFetch
            ? providerText("providers.copy.52")
            : providerText("providers.copy.53")}
        </p>
      )}
      {cached != null && (
        <ModelSearchList
          models={cached.models}
          suggested={cached.suggestedDefault}
          current={connection.defaultModel}
          searchTestId={`connection-models-search-${connection.id}`}
          pickTestId={(model) =>
            `connection-models-use-${connection.id}-${model}`
          }
          onPick={(model) => void onApplyDefault({ defaultModel: model })}
        />
      )}
      <div className="mt-2 flex flex-wrap items-end gap-2">
        <label className="flex min-w-40 flex-1 flex-col gap-1 text-label text-ink-500">
          {providerText("providers.copy.21")}
          <input
            data-testid={`connection-models-custom-${connection.id}`}
            value={custom}
            onChange={(event) => setCustom(event.target.value)}
            placeholder={connection.defaultModel}
            className="rounded-lg border border-ink-800 bg-ink-900 px-2.5 py-1.5 font-mono text-label text-ink-200"
          />
        </label>
        <button
          type="button"
          data-testid={`connection-models-custom-apply-${connection.id}`}
          disabled={custom.trim() === ""}
          onClick={() => {
            const next = custom.trim();
            setCustom("");
            void onApplyDefault({ defaultModel: next });
          }}
          className="rounded-lg border border-ink-700 px-3 py-1.5 text-label font-medium text-ink-200 transition-colors hover:border-ink-600 disabled:opacity-60"
        >
          {providerText("providers.copy.22")}
        </button>
      </div>
    </div>
  );
}

/// S-5 blocker 8: the production entry for cloud voices (A15 Option 3).
/// A voice is a provider preset id plus a name; the profile stores the
/// structural connection reference and the cast flow copies the resolved
/// tuple into the recorded binding.
function CloudVoiceForm({
  connection,
}: {
  connection: ProviderConnectionView;
}) {
  const [open, setOpen] = useState(false);
  const [name, setName] = useState("");
  const [voiceId, setVoiceId] = useState("");
  const [notice, setNotice] = useState<string | null>(null);

  const submit = async () => {
    try {
      await invoke("create_cloud_voice_profile", {
        connectionId: connection.id,
        name,
        voiceId,
      });
      setName("");
      setVoiceId("");
      setNotice(providerText("providers.copy.54"));
    } catch (raw) {
      setNotice(String(raw));
    }
  };

  if (!open) {
    return (
      <button
        type="button"
        data-testid={`connection-add-voice-${connection.id}`}
        onClick={() => setOpen(true)}
        className="text-label font-medium text-ink-400 transition-colors hover:text-ink-200"
      >
        {providerText("providers.copy.23")}
      </button>
    );
  }
  return (
    <div
      data-testid={`connection-voice-form-${connection.id}`}
      className="mt-2 flex w-full flex-wrap items-end gap-2 rounded-lg border border-ink-800 bg-ink-950 p-3"
    >
      <label className="flex min-w-32 flex-1 flex-col gap-1 text-label text-ink-500">
        {providerText("providers.copy.24")}
        <input
          data-testid={`connection-voice-name-${connection.id}`}
          value={name}
          onChange={(event) => setName(event.target.value)}
          className="rounded-lg border border-ink-800 bg-ink-900 px-2.5 py-1.5 text-secondary text-ink-200"
        />
      </label>
      <label className="flex min-w-40 flex-1 flex-col gap-1 text-label text-ink-500">
        {providerText("providers.copy.25")}
        <input
          data-testid={`connection-voice-id-${connection.id}`}
          value={voiceId}
          onChange={(event) => setVoiceId(event.target.value)}
          placeholder={providerText("providers.copy.66")}
          className="rounded-lg border border-ink-800 bg-ink-900 px-2.5 py-1.5 font-mono text-label text-ink-200"
        />
      </label>
      <button
        type="button"
        data-testid={`connection-voice-save-${connection.id}`}
        onClick={() => void submit()}
        className="rounded-lg bg-ink-100 px-3 py-1.5 text-label font-semibold text-ink-950 transition-colors hover:bg-ink-50"
      >
        {providerText("providers.copy.26")}
      </button>
      <button
        type="button"
        onClick={() => {
          setOpen(false);
          setNotice(null);
        }}
        className="text-label font-medium text-ink-500 transition-colors hover:text-ink-300"
      >
        {providerText("providers.copy.13")}
      </button>
      {notice != null && (
        <p
          data-testid={`connection-voice-notice-${connection.id}`}
          className="w-full text-label text-ink-400"
        >
          {notice}
        </p>
      )}
    </div>
  );
}

function ConsentSection({
  familyId,
  connections,
  onError,
}: {
  familyId: string;
  connections: ProviderConnectionView[];
  onError: (message: string) => void;
}) {
  const [consents, setConsents] = useState<CastConsentRow[]>([]);
  const [readFailed, setReadFailed] = useState(false);
  const load = useCallback(() => {
    invoke<CastConsentRow[]>("cast_consent_list")
      .then((all) => {
        setConsents(
          all.filter((row) =>
            connections.some((c) => c.id === row.connection_id),
          ),
        );
        setReadFailed(false);
      })
      // A read failure keeps whatever rows were already shown — blanking
      // the list would misread as "no consents" — and renders its own
      // retryable state, same treatment as the overview (round-3 item 1).
      .catch(() => setReadFailed(true));
  }, [connections]);
  useEffect(load, [load]);

  const active = consents.filter((row) => row.revoked_unix == null);
  if (active.length === 0 && !readFailed) return null;
  const name = (id: string) =>
    connections.find((c) => c.id === id)?.name ??
    providerText("providers.copy.55");
  return (
    <section
      data-testid={`consents-${familyId}`}
      className="rounded-2xl border border-ink-800 bg-ink-900 p-5"
    >
      <h3 className="text-body font-semibold text-ink-100">
        {providerText("providers.copy.27")}
      </h3>
      <p className="mt-1 text-label text-ink-500">
        {providerText("providers.copy.28")}
      </p>
      {readFailed && (
        <p
          data-testid="consents-load-failed"
          className="mt-2 flex items-center gap-3 rounded-lg border border-wash-red-ink/30 bg-wash-red px-3 py-2 text-label text-wash-red-ink"
        >
          {providerText("providers.copy.29")}
          {active.length > 0 && providerText("providers.copy.56")}.
          <button
            type="button"
            data-testid="consents-load-retry"
            onClick={load}
            className="ms-auto shrink-0 font-medium underline underline-offset-2"
          >
            {providerText("providers.copy.30")}
          </button>
        </p>
      )}
      <ul className="mt-2">
        {active.map((row) => (
          <li
            key={row.id}
            data-testid={`consent-row-${row.id}`}
            className="flex items-center gap-3 border-b border-ink-800/60 py-2.5 last:border-b-0"
          >
            <span className="min-w-0 flex-1 truncate text-secondary text-ink-300">
              <bdi>{name(row.connection_id)}</bdi> · {consentLabel(row.phase)} ·{" "}
              {row.data_categories.map(consentLabel).join(", ")} →{" "}
              <bdi dir="ltr" className="font-mono text-label">
                {row.hosts.join(", ")}
              </bdi>
            </span>
            <button
              type="button"
              data-testid={`consent-revoke-${row.id}`}
              onClick={() =>
                void invoke("cast_consent_revoke", { consentId: row.id })
                  .then(load)
                  .catch((raw) =>
                    onError(
                      providerText("providers.revokeFailed", {
                        reason: String(raw),
                      }),
                    ),
                  )
              }
              className="shrink-0 text-label font-medium text-wash-red-ink/80 transition-colors hover:text-wash-red-ink"
            >
              {providerText("providers.copy.31")}
            </button>
          </li>
        ))}
      </ul>
    </section>
  );
}

function AddConnectionForm({
  definition,
  family,
  onDone,
}: {
  definition: ProviderDefinition;
  family: ProviderFamilyView;
  onDone: () => void;
}) {
  const { t } = useI18n();
  const kinds =
    definition.id === "openai"
      ? ["openai_compatible", "openai_tts"]
      : definition.providerType
        ? [definition.providerType]
        : [];
  const [kindId, setKindId] = useState(kinds[0] ?? "");
  const [name, setName] = useState("");
  const [baseUrl, setBaseUrl] = useState(
    definition.id === "custom" ? "" : (definition.baseUrl ?? ""),
  );
  const [model, setModel] = useState("");
  const [keyless, setKeyless] = useState(
    definition.local && family.configuredKeys === 0,
  );
  const [error, setError] = useState<string | null>(null);
  const [listing, setListing] = useState(false);
  const [listed, setListed] = useState<{
    models: string[];
    suggestedDefault: string | null;
  } | null>(null);
  const listingRequest = useRef(0);
  const listingOperation = useRef<string | null>(null);
  useEffect(
    () => () => {
      listingRequest.current += 1;
      if (listingOperation.current)
        void invoke("provider_cancel_setup", {
          operationId: listingOperation.current,
        }).catch(() => undefined);
    },
    [],
  );
  const clearListing = () => {
    listingRequest.current += 1;
    if (listingOperation.current)
      void invoke("provider_cancel_setup", {
        operationId: listingOperation.current,
      }).catch(() => undefined);
    listingOperation.current = null;
    setListing(false);
    setListed(null);
    setModel("");
    setError(null);
  };
  if (kinds.length === 0) return null;
  const custom = definition.id === "custom";
  const needsUrl = kindId === "openai_compatible";
  const listModels = async () => {
    const request = ++listingRequest.current;
    setListing(true);
    setError(null);
    try {
      let fetched: { models: string[]; suggestedDefault: string | null };
      if (custom) {
        fetched = await invoke("provider_draft_models", {
          providerType: kindId,
          baseUrl,
          credentialNotRequired: keyless,
        });
      } else {
        listingOperation.current = crypto.randomUUID();
        const catalog = await invoke<
          import("./providerSetupModel").ProviderCatalog
        >("provider_refresh_catalog", {
          providerId: definition.id,
          operationId: listingOperation.current,
        });
        const role =
          kindId === "openai_tts" || kindId === "cartesia_tts"
            ? "narration"
            : "script";
        fetched = {
          models: catalog.models
            .filter((entry) => entry[role] !== "unsupported")
            .map((entry) => entry.id),
          suggestedDefault: catalog.recommendations[role],
        };
      }
      if (request !== listingRequest.current) return;
      setListed(fetched);
      if (!model.trim() && fetched.suggestedDefault)
        setModel(fetched.suggestedDefault);
    } catch {
      if (request === listingRequest.current)
        setError(t("providers.error.network"));
    } finally {
      if (request === listingRequest.current) {
        listingOperation.current = null;
        setListing(false);
      }
    }
  };
  const submit = async () => {
    try {
      await invoke("provider_connection_create", {
        name,
        providerType: kindId,
        baseUrl: needsUrl ? baseUrl : null,
        credentialNotRequired: needsUrl ? keyless : false,
        defaultModel: model,
      });
      setName("");
      setModel("");
      setListed(null);
      setError(null);
      onDone();
    } catch {
      setError(t("providers.error.connection"));
    }
  };
  return (
    <div data-testid="add-connection-form" className="mt-3 flex flex-col gap-3">
      <div className="flex flex-wrap items-end gap-3">
        {kinds.length > 1 && (
          <div className="flex flex-col gap-1 text-label text-ink-400">
            <label htmlFor="add-connection-kind">{t("providers.purpose")}</label>
            <CustomDropdown
              id="add-connection-kind"
              testId="add-connection-kind"
              label={t("providers.purpose")}
              value={kindId}
              onChange={(value) => {
                setKindId(value);
                clearListing();
              }}
              options={kinds.map((value) => ({
                value,
                label: t(
                  value === "openai_tts"
                    ? "providers.narration"
                    : "providers.scriptOnly",
                ),
              }))}
            />
          </div>
        )}
        <label className="flex min-w-40 flex-1 flex-col gap-1 text-label text-ink-400">
          {t("providers.name")}
          <input
            data-testid="add-connection-name"
            value={name}
            onChange={(event) => setName(event.target.value)}
            className="rounded-lg border border-ink-700 bg-ink-950 p-2 text-ink-100"
          />
        </label>
        {custom && (
          <label className="flex min-w-48 flex-1 flex-col gap-1 text-label text-ink-400">
            {t("providers.endpoint")}
            <input
              dir="ltr"
              data-testid="add-connection-url"
              value={baseUrl}
              onChange={(event) => {
                setBaseUrl(event.target.value);
                clearListing();
              }}
              className="rounded-lg border border-ink-700 bg-ink-950 p-2 font-mono text-ink-100"
            />
          </label>
        )}
        <label className="flex min-w-40 flex-1 flex-col gap-1 text-label text-ink-400">
          {t("providers.defaultModel")}
          <input
            dir="ltr"
            data-testid="add-connection-model"
            value={model}
            onChange={(event) => setModel(event.target.value)}
            className="rounded-lg border border-ink-700 bg-ink-950 p-2 font-mono text-ink-100"
          />
        </label>
        <button
          type="button"
          data-testid="add-connection-list-models"
          disabled={
            listing ||
            (custom
              ? !baseUrl.trim()
              : !family.configuredKeys && !definition.local)
          }
          onClick={() => void listModels()}
          className="rounded-lg border border-ink-700 px-3 py-2 text-label text-ink-100 disabled:opacity-50"
        >
          {t(listing ? "providers.checking" : "providers.listModels")}
        </button>
        <button
          type="button"
          data-testid="add-connection-submit"
          disabled={!name.trim() || !model.trim()}
          onClick={() => void submit()}
          className="rounded-lg bg-ink-100 px-3 py-2 text-label font-semibold text-ink-950 disabled:opacity-50"
        >
          {t("providers.addConnection")}
        </button>
      </div>
      {custom && (
        <label className="flex items-center gap-2 text-label text-ink-400">
          <input
            type="checkbox"
            data-testid="add-connection-keyless"
            checked={keyless}
            onChange={(event) => setKeyless(event.target.checked)}
          />
          {t("providers.keyless")}
        </label>
      )}
      {listed && (
        <div data-testid="add-connection-models-panel">
          <ModelSearchList
            models={listed.models}
            suggested={listed.suggestedDefault}
            current={model}
            searchTestId="add-connection-models-search"
            pickTestId={(id) => `add-connection-models-use-${id}`}
            onPick={setModel}
          />
        </div>
      )}
      {error && (
        <p
          data-testid="add-connection-error"
          role="alert"
          className="text-label text-wash-red-ink"
        >
          {error}
        </p>
      )}
    </div>
  );
}

/// S-5 blocker 2: the locked-vault remedy lives HERE — the one Providers
/// surface unlocks the vault or switches to session-only keys directly;
/// nothing points at a General panel that no longer exists.
function VaultRecovery({ onMutated }: { onMutated: () => void }) {
  const [passphrase, setPassphrase] = useState("");
  const [error, setError] = useState<string | null>(null);

  const unlock = async () => {
    try {
      await invoke("unlock_voice_key_vault", { passphrase });
      setPassphrase("");
      setError(null);
      onMutated();
    } catch (raw) {
      setError(String(raw));
    }
  };
  const sessionOnly = async () => {
    try {
      await invoke("use_session_only_voice_keys");
      setError(null);
      onMutated();
    } catch (raw) {
      setError(String(raw));
    }
  };

  return (
    <div
      data-testid="providers-vault-locked"
      className="flex flex-col gap-2 rounded-xl border border-wash-yellow-ink/30 bg-wash-yellow px-4 py-3 text-secondary text-wash-yellow-ink"
    >
      {providerText("providers.copy.32")}
      <div className="flex flex-wrap items-end gap-2">
        <label className="flex min-w-40 flex-col gap-1 text-label">
          {providerText("providers.copy.33")}
          <input
            type="password"
            data-testid="providers-vault-passphrase"
            value={passphrase}
            onChange={(event) => setPassphrase(event.target.value)}
            onKeyDown={(event) => {
              if (event.key === "Enter") void unlock();
            }}
            autoComplete="off"
            className="rounded-lg border border-ink-800 bg-ink-950 px-2.5 py-1.5 font-mono text-label text-ink-200"
          />
        </label>
        <button
          type="button"
          data-testid="providers-vault-unlock"
          onClick={() => void unlock()}
          className="rounded-lg bg-ink-100 px-3 py-1.5 text-label font-semibold text-ink-950 transition-colors hover:bg-ink-50"
        >
          {providerText("providers.copy.34")}
        </button>
        <button
          type="button"
          data-testid="providers-vault-session-only"
          onClick={() => void sessionOnly()}
          className="rounded-lg border border-ink-700 px-3 py-1.5 text-label font-medium text-ink-200 transition-colors hover:border-ink-600"
        >
          {providerText("providers.copy.35")}
        </button>
      </div>
      {error != null && (
        <p
          data-testid="providers-vault-error"
          className="text-label text-wash-red-ink"
        >
          {error}
        </p>
      )}
    </div>
  );
}

function ProviderDetail({
  definition,
  family,
  tests,
  onBack,
  onMutated,
  onError,
}: {
  definition: ProviderDefinition;
  family: ProviderFamilyView;
  tests: Record<string, PoolTestProgress>;
  onBack: () => void;
  onMutated: () => void;
  onError: (message: string) => void;
}) {
  const { t, dir } = useI18n();
  const [advanced, setAdvanced] = useState(definition.id === "custom");
  const heading = useRef<HTMLHeadingElement | null>(null);
  useEffect(() => {
    heading.current?.focus();
  }, []);
  return (
    <div
      data-testid={`provider-detail-${family.id}`}
      className="flex flex-col gap-5"
    >
      <nav
        aria-label={providerText("providers.copy.58")}
        className="flex items-center gap-2 text-secondary"
      >
        <button
          type="button"
          data-testid="providers-back"
          onClick={onBack}
          className="text-ink-400 transition-colors hover:text-ink-100"
        >
          {dir === "rtl" ? "→" : "←"} {t("providers.back")}
        </button>
        <span aria-hidden="true" className="text-ink-500">
          /
        </span>
        <span className="text-ink-200">
          {family.id === "custom"
            ? providerText("providers.customName")
            : family.displayName}
        </span>
      </nav>

      <header className="flex items-center gap-4">
        <ProviderMark family={family} />
        <div className="min-w-0">
          <h2
            ref={heading}
            tabIndex={-1}
            className="font-serif text-display font-semibold tracking-tight text-ink-50 outline-none"
          >
            {family.id === "custom"
              ? providerText("providers.customName")
              : family.displayName}
          </h2>
          <p className="text-secondary text-ink-400">
            {family.capabilities.map(capabilityLabel).join(" · ")} ·{" "}
            {cardLabelText(family.card)}
          </p>
        </div>
      </header>

      {family.vaultLocked && <VaultRecovery onMutated={onMutated} />}

      {definition.id !== "custom" && (
        <ProviderSetup
          key={definition.id}
          definition={definition}
          family={family}
          onMutated={onMutated}
          onAdvanced={() => setAdvanced((value) => !value)}
        />
      )}
      {advanced && (
        <>
          <section className="rounded-2xl border border-ink-800 bg-ink-900 p-5">
            <h3 className="text-body font-semibold text-ink-100">
              {t("providers.advancedTitle")}
            </h3>
            <p className="mt-1 text-label text-ink-500">
              {providerText("providers.copy.36")}
            </p>
            {family.connections.length > 0 && (
              <ul className="mt-2">
                {family.connections.map((connection) => (
                  <ConnectionRow
                    key={connection.id}
                    connection={connection}
                    onMutated={onMutated}
                  />
                ))}
              </ul>
            )}
            <AddConnectionForm
              definition={definition}
              family={family}
              onDone={onMutated}
            />
          </section>

          {(family.pools ?? []).map((pool) => (
            <PoolSection
              key={pool.baseUrl}
              family={family}
              pool={pool}
              test={
                tests[
                  poolTestKey(family.custodyId ?? family.id, pool.baseUrl)
                ] ?? null
              }
              onMutated={onMutated}
              onError={onError}
            />
          ))}

          <ConsentSection
            familyId={family.custodyId ?? family.id}
            connections={family.connections}
            onError={onError}
          />
        </>
      )}
      {family.retentionLinks.length > 0 && (
        <section
          data-testid={`retention-${family.id}`}
          className="rounded-2xl border border-ink-800 bg-ink-900 p-5"
        >
          <h3 className="text-body font-semibold text-ink-100">
            {t("providers.dataHandling")}
          </h3>
          <p className="mt-1 text-label text-ink-500">
            {t("providers.retention")}
          </p>
          <ul className="mt-2 flex flex-col gap-1">
            {family.retentionLinks.map((link) => (
              <li key={link.kind}>
                <button
                  type="button"
                  onClick={() => void openUrl(link.url)}
                  dir="ltr"
                  className="break-all text-start text-label font-medium text-ember-400 underline underline-offset-2 transition-colors hover:text-ember-300"
                >
                  {link.url}
                </button>
              </li>
            ))}
          </ul>
        </section>
      )}
    </div>
  );
}

/// The sub-page's real address (re-verdict item 5): `#providers/<id>` in
/// the location hash. One reader for mount and popstate.
function providerFromHash(): string | null {
  const match = /^#providers\/([\w-]+)$/.exec(window.location.hash);
  return match?.[1] ?? null;
}

function ProvidersPage() {
  const { t } = useI18n();
  const [families, setFamilies] = useState<ProviderFamilyView[] | null>(null);
  const [directory, setDirectory] = useState<ProviderDefinition[]>([]);
  // The address is the truth: a deep link or a pending in-app navigation
  // both land as the hash, and Back/Forward drive `selected` via popstate.
  const [selected, setSelected] = useState<string | null>(
    () => providerFromHash() ?? consumePendingProvider(),
  );
  const [tests, setTests] = useState<Record<string, PoolTestProgress>>({});
  const [pageError, setPageError] = useState<string | null>(null);
  const [loadFailed, setLoadFailed] = useState(false);
  const lastOpened = useRef<string | null>(null);

  // Page entry is the ONLY implicit data load; there is no polling and no
  // ambient probe — probes run only from explicit Test/Check actions.
  // A read failure KEEPS whatever was already rendered (blanking would
  // misread as "nothing configured") and offers an explicit retry.
  const load = useCallback(() => {
    Promise.all([
      invoke<ProviderDefinition[]>("provider_directory"),
      invoke<ProviderFamilyView[]>("providers_overview"),
    ])
      .then(([definitions, next]) => {
        setDirectory(definitions);
        setFamilies(projectProviders(definitions, next));
        setLoadFailed(false);
      })
      .catch(() => setLoadFailed(true));
  }, []);
  useEffect(load, [load]);

  const mutated = useCallback(() => {
    load();
    notifyVoiceKeysChanged();
  }, [load]);

  useEffect(() => {
    const unlisten = listen<PoolTestProgress>(TEST_PROGRESS_EVENT, (event) => {
      setTests((current) => ({
        ...current,
        [poolTestKey(event.payload.provider, event.payload.baseUrl)]:
          event.payload,
      }));
      // Live counters render straight from the streamed event; the full
      // typed projection is re-read ONCE, at terminal settlement — never
      // per probe (re-verdict item 6).
      if (event.payload.done) load();
    });
    return () => {
      void unlisten.then((fn) => fn());
    };
  }, [load]);

  // History wiring: opening a detail pushes its address; Back/Forward and
  // Escape move through real history entries; returning to the cards
  // restores focus to the card that opened the detail (sub-page IA gate).
  useEffect(() => {
    const initial = selected;
    if (initial != null && providerFromHash() == null) {
      // A pending in-app navigation: give the cards their own entry so
      // Back/Escape return there instead of leaving the app view.
      window.history.replaceState(null, "", "#");
      window.history.pushState(
        { providersDetail: initial },
        "",
        `#providers/${initial}`,
      );
      lastOpened.current = initial;
    }
    const onPop = () => {
      const id = providerFromHash();
      setSelected(id);
      if (id == null && lastOpened.current != null) {
        const opener = lastOpened.current;
        requestAnimationFrame(() => {
          document
            .querySelector<HTMLElement>(
              `[data-testid="provider-card-${opener}"]`,
            )
            ?.focus();
        });
      }
    };
    window.addEventListener("popstate", onPop);
    return () => {
      window.removeEventListener("popstate", onPop);
      // Leaving the Providers view: a stale detail address must not
      // re-open on an unrelated later mount.
      if (window.location.hash.startsWith("#providers")) {
        window.history.replaceState(null, "", "#");
      }
    };
    // Mount-only: `selected`'s initial value is deliberately captured once.
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, []);

  const openDetail = useCallback((id: string) => {
    lastOpened.current = id;
    window.history.pushState({ providersDetail: id }, "", `#providers/${id}`);
    setSelected(id);
  }, []);

  useEffect(() => {
    const openRequested = () => {
      const id = consumePendingProvider();
      if (id != null) openDetail(id);
    };
    window.addEventListener(GO_VIEW_EVENT, openRequested);
    return () => window.removeEventListener(GO_VIEW_EVENT, openRequested);
  }, [openDetail]);

  const closeDetail = useCallback(() => {
    if (providerFromHash() != null) {
      window.history.back();
    } else {
      setSelected(null);
    }
  }, []);

  // Escape returns from the sub-page through the same history entry the
  // breadcrumb uses (Back-safe addressable state).
  useEffect(() => {
    if (selected == null) return;
    const onKey = (event: KeyboardEvent) => {
      if (event.key === "Escape") closeDetail();
    };
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
  }, [selected, closeDetail]);

  const selectedId = selected === "openai_compat" ? "custom" : selected;
  const detail = families?.find((family) => family.id === selectedId);
  const detailDefinition = directory.find((entry) => entry.id === selectedId);

  return (
    <section
      data-testid="providers-page"
      className="flex w-full flex-col gap-6"
    >
      {pageError != null && (
        <p
          data-testid="providers-error"
          className="flex items-center gap-3 rounded-xl border border-wash-red-ink/30 bg-wash-red px-4 py-3 text-secondary text-wash-red-ink"
        >
          {pageError}
          <button
            type="button"
            data-testid="providers-error-dismiss"
            onClick={() => setPageError(null)}
            className="ms-auto shrink-0 font-medium underline underline-offset-2"
          >
            {providerText("providers.copy.37")}
          </button>
        </p>
      )}
      {loadFailed && (
        <p
          data-testid="providers-load-failed"
          className="flex items-center gap-3 rounded-xl border border-wash-red-ink/30 bg-wash-red px-4 py-3 text-secondary text-wash-red-ink"
        >
          {providerText("providers.copy.38")}
          {families != null && providerText("providers.copy.57")}.
          <button
            type="button"
            data-testid="providers-load-retry"
            onClick={load}
            className="ms-auto shrink-0 font-medium underline underline-offset-2"
          >
            {providerText("providers.copy.30")}
          </button>
        </p>
      )}
      {detail != null && detailDefinition != null ? (
        <ProviderDetail
          key={detail.id}
          definition={detailDefinition}
          family={detail}
          tests={tests}
          onBack={closeDetail}
          onMutated={mutated}
          onError={setPageError}
        />
      ) : (
        <>
          <header className="flex flex-col gap-1.5">
            <h2 className="font-serif text-title font-semibold tracking-tight text-ink-50">
              {t("nav.providers")}
            </h2>
            <p className="max-w-xl text-body text-ink-400">
              {t("providers.intro")}
            </p>
          </header>
          {families == null ? (
            <div
              aria-hidden="true"
              data-testid="providers-skeleton"
              className="flex flex-col gap-3"
            >
              {[0, 1, 2].map((row) => (
                <div
                  key={row}
                  className="shimmer-bar h-20 rounded-2xl bg-ink-900"
                />
              ))}
            </div>
          ) : (
            <div className="flex flex-col gap-3" data-testid="provider-cards">
              {(["main", "others", "local"] as const).map((group) => (
                <section
                  key={group}
                  data-testid={`provider-group-${group}`}
                  className="flex flex-col gap-3"
                >
                  <h3 className="mt-3 text-body font-semibold text-ink-200">
                    {t(`providers.group.${group}`)}
                    <span className="ms-3 text-label font-normal text-ink-500">
                      {t("providers.groupCount", {
                        count: families.filter(
                          (family) =>
                            directory.find((entry) => entry.id === family.id)
                              ?.group === group &&
                            (family.configuredKeys > 0 ||
                              family.connections.some(
                                (connection) =>
                                  connection.enabled && !connection.keyRequired,
                              )),
                        ).length,
                      })}
                    </span>
                  </h3>
                  {families
                    .filter(
                      (family) =>
                        directory.find((entry) => entry.id === family.id)
                          ?.group === group,
                    )
                    .map((family) => (
                      <ProviderCard
                        key={family.id}
                        family={family}
                        onOpen={() => openDetail(family.id)}
                      />
                    ))}
                </section>
              ))}
            </div>
          )}
          <p className="text-label text-ink-500">{t("providers.trademarks")}</p>
        </>
      )}
    </section>
  );
}

export default ProvidersPage;
