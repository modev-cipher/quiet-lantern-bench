import { useMemo } from "react";
import { useI18n } from "./i18n";
import { CustomDropdown } from "./CustomDropdown";
import VoicePreviewButton from "./VoicePreviewButton";
import { AlertIcon } from "./Icons";
import { CostFact } from "./CostFact";
import { LOOPBACK_FACT, type CastCloudVoiceOption } from "./castProviderModel";
import { groupVoices, voiceLabel, voiceMeta } from "./voiceModel";
import { advisoryCopy, type LanguageMismatch } from "./languageAdvisoryModel";
import { goToView } from "./views";
import type {
  FitBadgeKind,
  JobMode,
  VoiceProfileSummary,
} from "./jobFormModel";
import type { ProviderOption } from "./castModelSelectionModel";
import { ensureEngine } from "./engineSetupStore";
import SegmentedSwitch from "./ui/SegmentedSwitch";

const FIT_STYLES: Record<FitBadgeKind, string> = {
  green: "bg-wash-green text-wash-green-ink",
  yellow: "bg-wash-yellow text-wash-yellow-ink",
  orange: "bg-ember-500/15 text-ember-300",
  red: "bg-wash-red text-wash-red-ink",
};

export interface LocalEngineOption {
  id: string;
  name: string;
  badge?: FitBadgeKind;
  nonCommercial?: boolean;
}

export interface VoiceSelectorProps {
  // Local voice options & state
  usableVoices: VoiceProfileSummary[];
  selectedLocalVoiceId: string | null;
  onSelectLocalVoice: (voiceId: string) => void;

  // Local engine options & state for two-tier selection
  localEngines?: LocalEngineOption[];
  selectedEngineId?: string | null;
  onSelectEngine?: (engineId: string) => void;
  preferredLanguage?: string | null;

  // Cloud options & state
  cloudVoices: CastCloudVoiceOption[];
  cloudStatus?: "loading" | "error" | "ready";
  cloudProviders: ProviderOption[];
  selectedCloudProviderId: string | null;
  selectedCloudVoiceId: string | null;
  onSelectCloudProvider: (connectionId: string) => void;
  onSelectCloudVoice: (voiceProfileId: string) => void;

  // Engine selection
  engineMode: "local" | "cloud";
  onSwitchEngine: (mode: "local" | "cloud") => void;

  // Auxiliary UI & actions
  fit?: { kind: FitBadgeKind; label: string } | null;
  voiceMismatch: (voice: VoiceProfileSummary) => LanguageMismatch | null;
  selectedMismatch?: LanguageMismatch | null;
  alignAdvisory?: boolean;
  onOpenModels: () => void;
  onCloneVoice: () => void;
  onOpenProviders?: () => void;

  // Cloud facts
  activeCloudVoice?: CastCloudVoiceOption | null;
  bookId: string;
  scopeArg: unknown;
  mode: JobMode;
}

export function VoiceSelector({
  usableVoices,
  selectedLocalVoiceId,
  onSelectLocalVoice,
  localEngines,
  selectedEngineId,
  onSelectEngine,
  preferredLanguage,
  cloudVoices,
  cloudStatus = "ready",
  cloudProviders,
  selectedCloudProviderId,
  selectedCloudVoiceId,
  onSelectCloudProvider,
  onSelectCloudVoice,
  engineMode,
  onSwitchEngine,
  fit,
  voiceMismatch,
  selectedMismatch,
  alignAdvisory = false,
  onOpenModels,
  onCloneVoice,
  onOpenProviders,
  activeCloudVoice,
  bookId,
  scopeArg,
  mode,
}: VoiceSelectorProps) {
  const { t } = useI18n();

  const filteredVoices = useMemo(() => {
    if (!selectedEngineId || !localEngines || localEngines.length <= 1) {
      return usableVoices;
    }
    const matching = usableVoices.filter((v) => v.engine === selectedEngineId);
    return matching.length > 0 ? matching : usableVoices;
  }, [usableVoices, selectedEngineId, localEngines]);

  const selectedLocalVoice = useMemo(
    () => usableVoices.find((v) => v.id === selectedLocalVoiceId) ?? null,
    [usableVoices, selectedLocalVoiceId],
  );

  const selectedVoiceMeta = useMemo(
    () => voiceMeta(selectedLocalVoice?.presetVoiceId),
    [selectedLocalVoice?.presetVoiceId],
  );

  const currentProviderVoices = useMemo(
    () =>
      selectedCloudProviderId != null
        ? cloudVoices.filter((v) => v.connectionId === selectedCloudProviderId)
        : [],
    [cloudVoices, selectedCloudProviderId],
  );

  const effectiveSelectedCloudVoiceId =
    selectedCloudVoiceId ?? currentProviderVoices[0]?.voiceProfileId ?? null;

  return (
    <section className="space-y-3">
      <div className="flex flex-wrap items-center gap-3">
        <div className="w-64">
          <SegmentedSwitch
            label={t("cast.voice")}
            testId="voice-engine-segmented"
            value={engineMode}
            onChange={onSwitchEngine}
            options={[
              {
                value: "local",
                label: t("cast.engine.local"),
                testId: "voice-engine-local",
              },
              {
                value: "cloud",
                label: t("cast.engine.cloud"),
                testId: "voice-engine-cloud",
              },
            ]}
          />
        </div>
        {fit && engineMode === "local" && (
          <span
            data-testid="voice-fit"
            className={`rounded px-1.5 py-0.5 text-label font-medium ${FIT_STYLES[fit.kind]}`}
          >
            {fit.label}
          </span>
        )}
      </div>

      {engineMode === "local" ? (
        <div className="space-y-3">
          {localEngines && localEngines.length > 1 && onSelectEngine && (
            <div className="space-y-1">
              <label
                htmlFor="voice-engine-select"
                className="block text-label font-medium text-ink-400"
              >
                {t("cast.model")}
              </label>
              <CustomDropdown
                id="voice-engine-select"
                testId="voice-engine-select"
                label={t("cast.model")}
                className="w-full"
                value={selectedEngineId ?? ""}
                onChange={onSelectEngine}
                options={localEngines.map((engine) => ({
                  value: engine.id,
                  label: engine.name,
                  badge: engine.nonCommercial
                    ? t("models.nonCommercial")
                    : engine.badge
                      ? t(`fit.${engine.badge}.label`)
                      : undefined,
                }))}
                placeholder={t("cast.model")}
              />
            </div>
          )}

          {usableVoices.length > 0 && (
            <div className="space-y-2">
              <CustomDropdown
                id="voice-select"
                testId="voice-select"
                label={t("cast.voice")}
                className="w-full"
                value={selectedLocalVoiceId ?? ""}
                onChange={onSelectLocalVoice}
                options={groupVoices(filteredVoices).flatMap((group) =>
                  group.voices.map((voice) => ({
                    value: voice.id,
                    label:
                      voiceMismatch(voice) != null
                        ? `⚠ ${voiceLabel(voice)}`
                        : voiceLabel(voice),
                    group: `${group.flag} ${group.language}`,
                  })),
                )}
                placeholder={t("cast.noVoices")}
              />
              <div className="flex items-center gap-2">
                {selectedLocalVoice != null && (
                  <VoicePreviewButton
                    voiceProfileId={selectedLocalVoice.id}
                    language={preferredLanguage ?? undefined}
                  />
                )}
                <button
                  type="button"
                  data-testid="clone-voice-open"
                  onClick={onCloneVoice}
                  className="shrink-0 rounded-lg border border-ink-800 px-3 py-1.5 text-body text-ink-200 transition-colors hover:border-ember-500/60 hover:text-ink-100"
                >
                  {t("voice.clone.title")}
                </button>
              </div>
            </div>
          )}

          {(usableVoices.length === 0 || selectedLocalVoiceId == null) && (
            <p
              data-testid="voice-required"
              className="flex flex-wrap items-center gap-2 rounded-xl border border-ember-500/30 bg-ember-500/[0.07] px-3.5 py-2.5 text-secondary text-ember-200"
            >
              {t("cast.voiceRequired")}
              <button
                type="button"
                onClick={onOpenModels}
                className="font-semibold text-ember-300 underline underline-offset-2 transition-colors hover:text-ember-200"
              >
                {t("cast.installVoice")}
              </button>
            </p>
          )}

          {selectedVoiceMeta && (
            <p data-testid="voice-meta" className="text-secondary text-ink-500">
              {t(
                selectedVoiceMeta.gender === "female"
                  ? "voice.meta.female"
                  : "voice.meta.male",
                {
                  flag: selectedVoiceMeta.flag,
                  name: selectedVoiceMeta.shortName,
                  language: selectedVoiceMeta.language,
                },
              )}
            </p>
          )}
        </div>
      ) : (
        <div className="space-y-2">
          {cloudStatus !== "ready" ? (
            cloudStatus === "loading" ? (
              <p role="status" className="text-secondary text-ink-400">
                {t("models.refreshing")}
              </p>
            ) : null
          ) : cloudProviders.length === 0 ? (
            <div
              data-testid="cast-no-cloud-voices"
              className="rounded-xl border border-ink-800 bg-ink-950 p-4 text-center space-y-2"
            >
              <p className="text-secondary text-ink-400">
                {t("cast.noCloudVoices")}
              </p>
              <button
                type="button"
                data-testid="cast-open-providers-btn"
                onClick={onOpenProviders ?? (() => goToView("providers"))}
                className="text-secondary font-medium text-ember-400 transition-colors hover:text-ember-300 underline underline-offset-2"
              >
                {t("cast.connectProviders")}
              </button>
            </div>
          ) : (
            <div className="space-y-3">
              <div>
                <label
                  htmlFor="cast-cascade-provider-tts"
                  className="block text-label font-medium text-ink-400 mb-1"
                >
                  {t("cast.voiceProvider")}
                </label>
                <CustomDropdown
                  id="cast-cascade-provider-tts"
                  testId="cast-cascade-provider-tts"
                  label={t("cast.voiceProvider")}
                  value={selectedCloudProviderId ?? ""}
                  onChange={onSelectCloudProvider}
                  options={cloudProviders.map((p) => ({
                    value: p.id,
                    label: p.name,
                    badge: t("cast.engine.cloud"),
                  }))}
                />
              </div>
              <div>
                <label
                  htmlFor="cast-cascade-model-tts"
                  className="block text-label font-medium text-ink-400 mb-1"
                >
                  {t("cast.voice")}
                </label>
                <CustomDropdown
                  id="cast-cascade-model-tts"
                  testId="cast-cascade-model-tts"
                  label={t("cast.voice")}
                  value={effectiveSelectedCloudVoiceId ?? ""}
                  onChange={onSelectCloudVoice}
                  options={currentProviderVoices.map((v) => ({
                    value: v.voiceProfileId,
                    label: v.name,
                    sublabel: `${v.model} · ${v.voiceId}`,
                    badge: t("cast.engine.cloud"),
                  }))}
                />
              </div>

              {activeCloudVoice != null && (
                <div className="space-y-2">
                  {activeCloudVoice.loopback ? (
                    <p
                      data-testid="cast-loopback-tts"
                      className="text-label text-ink-500"
                    >
                      {LOOPBACK_FACT()}
                    </p>
                  ) : (
                    <CostFact
                      phase="tts"
                      connectionId={activeCloudVoice.connectionId}
                      model={activeCloudVoice.model}
                      bookId={bookId}
                      scope={scopeArg}
                      mode={mode}
                    />
                  )}
                </div>
              )}

              {alignAdvisory && (
                <p
                  data-testid="cast-align-advisory"
                  className="rounded-lg border border-ink-800 bg-ink-950 p-2.5 text-label text-ink-400"
                >
                  {t("voice.alignAdvisory")}{" "}
                  <button
                    type="button"
                    data-testid="cast-align-advisory-models"
                    onClick={() =>
                      void ensureEngine("localAi", onOpenModels).then(
                        (ready) => {
                          if (ready) onOpenModels();
                        },
                      )
                    }
                    className="font-medium text-ember-400 underline underline-offset-2 transition-colors hover:text-ember-300"
                  >
                    {t("voice.installAlignment")}
                  </button>
                </p>
              )}
            </div>
          )}
        </div>
      )}

      {selectedMismatch != null && (
        <div
          role="status"
          data-testid="language-advisory"
          className="flex items-start gap-2.5 rounded-xl border border-ember-500/30 bg-ember-500/[0.07] px-3.5 py-2.5 text-secondary text-ember-200"
        >
          <span className="mt-0.5 shrink-0 text-ember-300">
            <AlertIcon className="h-4 w-4" />
          </span>
          <span>{advisoryCopy(selectedMismatch)}</span>
        </div>
      )}
    </section>
  );
}
