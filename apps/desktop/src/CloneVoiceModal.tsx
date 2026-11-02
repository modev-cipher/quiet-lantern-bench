import { useCallback, useEffect, useRef, useState } from "react";
import { invoke } from "@tauri-apps/api/core";
import { open } from "@tauri-apps/plugin-dialog";

import { engineRefusal } from "./engineSetupModel";
import { openEngineSetup } from "./engineSetupStore";
import { CloseIcon } from "./Icons";
import { CustomDropdown } from "./CustomDropdown";
import { useDialogFocus } from "./useDialogFocus";
import { goToModels } from "./views";
import type { FitBadgeKind, VoiceProfileSummary } from "./jobFormModel";
import { forgetMediaUrl, getMediaUrl } from "./mediaUrl";
import {
  canRecord,
  canSave,
  defaultName,
  discardSample,
  failed,
  initialState,
  inputsUnavailable,
  nameError,
  sampleReady,
  sampleSummaryParams,
  sampleWarningKey,
  MAX_SECONDS,
  selectInput,
  type AudioInput,
  type CloneState,
  type VoiceSample,
} from "./cloneVoiceModel";
import { useI18n } from "./i18n";

interface RawSample {
  sampleId: string;
  seconds: number;
  hitHardStop: boolean;
}

export interface CloningEngineOption {
  id: string;
  name: string;
  diskBytes?: number;
  badge?: FitBadgeKind;
  nonCommercial?: boolean;
}

export default function CloneVoiceModal({
  open: isOpen,
  cloningReady,
  existing,
  cloningEngines,
  initialEngine,
  targetLanguage,
  cloningModelSize,
  onClose,
  onSaved,
}: {
  open: boolean;
  cloningReady: boolean;
  existing: VoiceProfileSummary[];
  cloningEngines?: CloningEngineOption[];
  initialEngine?: string | null;
  targetLanguage?: string | null;
  cloningModelSize?: string | null;
  onClose: () => void;
  onSaved: (profile: VoiceProfileSummary) => void;
}) {
  const { t, tx } = useI18n();
  const [state, setState] = useState<CloneState>(initialState);
  const [selectedEngine, setSelectedEngine] = useState<string | null>(
    initialEngine ?? (cloningEngines?.[0]?.id ?? null),
  );
  const [recording, setRecording] = useState(false);
  const dialogRef = useRef<HTMLDivElement>(null);
  const audioRef = useRef<HTMLAudioElement | null>(null);

  useEffect(
    () => () => {
      if (audioRef.current) {
        audioRef.current.pause();
        audioRef.current = null;
      }
      void invoke("cancel_voice_recording").catch(() => {});
    },
    [],
  );

  useEffect(() => {
    if (!isOpen) {
      if (audioRef.current) {
        audioRef.current.pause();
        audioRef.current = null;
      }
      void invoke("cancel_voice_recording").catch(() => {});
      return;
    }
    setState(initialState());
    setRecording(false);
    if (!cloningReady) return;
    invoke<AudioInput[]>("list_audio_inputs")
      .then((inputs) => setState((prev) => selectInput(prev, inputs)))
      .catch((err: unknown) =>
        setState((prev) => inputsUnavailable(prev, String(err))),
      );
  }, [isOpen, cloningReady]);

  useEffect(() => {
    if (initialEngine) {
      setSelectedEngine(initialEngine);
    } else if (cloningEngines && cloningEngines.length > 0) {
      setSelectedEngine((curr) =>
        curr && cloningEngines.some((e) => e.id === curr)
          ? curr
          : cloningEngines[0]!.id,
      );
    }
  }, [initialEngine, cloningEngines]);

  const adopt = useCallback(
    (raw: RawSample, source: VoiceSample["source"]) =>
      setState((prev) =>
        sampleReady(prev, { ...raw, source }, defaultName(existing)),
      ),
    [existing],
  );

  const startRecording = useCallback(async () => {
    setState((prev) => ({ ...prev, busy: true, error: null }));
    try {
      await invoke<string>("start_voice_recording", {
        deviceName: state.selectedInput,
      });
      setRecording(true);
      setState((prev) => ({ ...prev, step: "capturing", busy: false }));
    } catch (err) {
      setState((prev) => failed(prev, String(err)));
    }
  }, [state.selectedInput]);

  const stopRecording = useCallback(async () => {
    setState((prev) => ({ ...prev, busy: true }));
    try {
      const raw = await invoke<RawSample>("stop_voice_recording");
      setRecording(false);
      adopt(raw, "recording");
    } catch (err) {
      setRecording(false);
      setState((prev) => failed(prev, String(err)));
    }
  }, [adopt]);

  const importFile = useCallback(async () => {
    try {
      const picked = await open({
        multiple: false,
        filters: [
          { name: "Audio", extensions: ["wav", "mp3", "m4a", "flac", "ogg"] },
        ],
      });
      if (typeof picked !== "string") return;
      setState((prev) => ({ ...prev, busy: true, error: null }));
      const raw = await invoke<RawSample>("import_voice_sample", {
        path: picked,
      });
      adopt(raw, "import");
    } catch (err) {
      setState((prev) => failed(prev, String(err)));
    }
  }, [adopt]);

  const preview = useCallback(async () => {
    if (state.sample == null) return;
    setState((prev) => ({ ...prev, busy: true, error: null }));
    try {
      const wavPath = await invoke<string>("preview_voice_sample", {
        sampleId: state.sample.sampleId,
        engine: selectedEngine ?? undefined,
        language: targetLanguage ?? undefined,
      });
      const url = await getMediaUrl(wavPath);
      if (audioRef.current) {
        audioRef.current.pause();
        audioRef.current = null;
      }
      const audio = new Audio(url);
      audioRef.current = audio;
      void audio.play();
      setState((prev) => ({ ...prev, busy: false, previewPath: wavPath }));
    } catch (err) {
      const need = engineRefusal(String(err));
      if (need != null) {
        setState((prev) => ({ ...prev, busy: false }));
        openEngineSetup(need);
        return;
      }
      setState((prev) => failed(prev, String(err)));
    }
  }, [state.sample, selectedEngine, targetLanguage]);

  const discard = useCallback(async () => {
    if (audioRef.current) {
      audioRef.current.pause();
      audioRef.current = null;
    }
    if (state.previewPath) {
      forgetMediaUrl(state.previewPath);
    }
    const sampleId = state.sample?.sampleId;
    setState(discardSample);
    setRecording(false);
    await invoke("cancel_voice_recording").catch(() => {});
    if (sampleId != null) {
      await invoke("discard_voice_sample", { sampleId }).catch(() => {});
    }
  }, [state.sample, state.previewPath]);

  const save = useCallback(async () => {
    if (state.sample == null) return;
    setState((prev) => ({ ...prev, busy: true, error: null, step: "saving" }));
    try {
      const profile = await invoke<VoiceProfileSummary>("save_voice_profile", {
        sampleId: state.sample.sampleId,
        name: state.name.trim(),
        engine: selectedEngine ?? undefined,
      });
      onSaved(profile);
      onClose();
    } catch (err) {
      setState((prev) => failed(prev, String(err)));
    }
  }, [state.sample, state.name, selectedEngine, onSaved, onClose]);

  useDialogFocus(dialogRef, isOpen, {
    onEscape: () => void discard().then(onClose),
  });

  if (!isOpen) return null;

  const invalidName =
    state.sample != null ? nameError(state.name, existing) : null;
  const warningKey =
    state.sample != null ? sampleWarningKey(state.sample) : null;

  return (
    <div
      className="fixed inset-0 z-50 flex items-center justify-center bg-ink-950/80 p-4 backdrop-blur-sm"
      data-testid="clone-voice-modal"
      data-modal-layer=""
    >
      <div
        ref={dialogRef}
        role="dialog"
        aria-modal="true"
        aria-label={t("voice.clone.title")}
        tabIndex={-1}
        className="animate-modal-in w-full max-w-lg space-y-4 rounded-2xl border border-ink-700 bg-ink-950 p-6 shadow-overlay outline-none"
      >
        <div className="flex items-start justify-between gap-4">
          <h2 className="text-lg font-semibold text-ink-100">
            {t("voice.clone.title")}
          </h2>
          <button
            type="button"
            aria-label={t("voice.clone.close")}
            data-testid="clone-close"
            onClick={() => void discard().then(onClose)}
            className="text-ink-500 transition-colors hover:text-ink-200"
          >
            <CloseIcon />
          </button>
        </div>

        {!cloningReady ? (
          <div className="space-y-3" data-testid="clone-needs-model">
            <p className="text-body font-medium text-ink-100">
              {t("voice.clone.needsModelTitle")}
            </p>
            <p className="text-body text-ink-400">
              {tx("voice.clone.needsModelDetail", { size: "3.2 GB" })}
              {tx("voice.clone.needsModelDetail", {
                size: cloningModelSize ?? "3.2 GB",
              })}
            </p>
            <button
              type="button"
              data-testid="clone-open-models"
              onClick={() => {
                goToModels();
                onClose();
              }}
              className="rounded-lg bg-ink-50 px-3 py-2 text-body font-medium text-ink-950 transition-colors hover:bg-ink-150"
            >
              {t("voice.clone.openModels")}
            </button>
          </div>
        ) : (
          <>
            {state.sample == null ? (
              <div className="space-y-3">
                {cloningEngines && cloningEngines.length > 1 && (
                  <div className="space-y-1">
                    <label
                      htmlFor="clone-engine-select"
                      className="block text-secondary uppercase tracking-wide text-ink-500"
                    >
                      {t("cast.model")}
                    </label>
                    <CustomDropdown
                      id="clone-engine-select"
                      testId="clone-engine-select"
                      label={t("cast.model")}
                      value={selectedEngine ?? cloningEngines[0]!.id}
                      onChange={(val) => setSelectedEngine(val)}
                      options={cloningEngines.map((e) => ({
                        value: e.id,
                        label: e.name,
                        badge: e.nonCommercial
                          ? t("models.nonCommercial")
                          : (e.badge ? t(`fit.${e.badge}.label`) : undefined),
                      }))}
                    />
                  </div>
                )}

                <p className="text-body text-ink-400">
                  {t("voice.clone.recordPrompt")}
                </p>
                <p
                  className="rounded-lg border border-ink-800 bg-ink-900 p-3 text-body text-ink-200"
                  data-testid="clone-script"
                >
                  {t("voice.clone.recordScript")}
                </p>

                {state.inputsError != null && (
                  <p
                    className="text-body text-ember-300"
                    data-testid="clone-no-microphone"
                  >
                    {t(state.inputsErrorKey ?? "voice.clone.err.noInputs")}
                  </p>
                )}

                {state.inputs.length > 1 && (
                  <CustomDropdown
                    testId="clone-input-select"
                    label={t("voice.clone.microphone")}
                    value={state.selectedInput ?? ""}
                    onChange={(val) =>
                      setState((prev) => ({
                        ...prev,
                        selectedInput: val,
                      }))
                    }
                    options={state.inputs.map((input) => ({
                      value: input.name,
                      label: input.name,
                    }))}
                  />
                )}

                <div className="flex flex-wrap gap-2">
                  {recording ? (
                    <button
                      type="button"
                      data-testid="clone-stop"
                      onClick={() => void stopRecording()}
                      disabled={state.busy}
                      className="rounded-lg bg-ink-50 px-3 py-2 text-body font-medium text-ink-950 transition-colors hover:bg-ink-150 disabled:opacity-50"
                    >
                      {t("voice.clone.stopRecording")}
                    </button>
                  ) : (
                    <button
                      type="button"
                      data-testid="clone-record"
                      onClick={() => void startRecording()}
                      disabled={!canRecord(state)}
                      className="rounded-lg bg-ink-50 px-3 py-2 text-body font-medium text-ink-950 transition-colors hover:bg-ink-150 disabled:opacity-50"
                    >
                      {t("voice.clone.record")}
                    </button>
                  )}
                  <button
                    type="button"
                    data-testid="clone-import"
                    onClick={() => void importFile()}
                    disabled={state.busy}
                    className="rounded-lg border border-ink-800 px-3 py-2 text-body text-ink-200 disabled:opacity-50"
                  >
                    {t("voice.clone.importFile")}
                  </button>
                </div>
              </div>
            ) : (
              <div className="space-y-3">
                <p
                  className="text-body text-ink-200"
                  data-testid="clone-sample"
                >
                  {tx(
                    "voice.clone.sampleSummary",
                    sampleSummaryParams(state.sample),
                  )}
                </p>
                {warningKey != null && (
                  <p
                    className="text-secondary text-ink-400"
                    data-testid="clone-sample-warning"
                  >
                    {tx(warningKey!, { n: MAX_SECONDS })}
                  </p>
                )}
                <label className="block space-y-1">
                  <span className="text-secondary uppercase tracking-wide text-ink-500">
                    {t("voice.clone.name")}
                  </span>
                  <input
                    data-testid="clone-name"
                    value={state.name}
                    onChange={(event) =>
                      setState((prev) => ({
                        ...prev,
                        name: event.target.value,
                      }))
                    }
                    className="w-full rounded-lg border border-ink-800 bg-ink-950 px-3 py-2 text-body text-ink-100"
                  />
                </label>
                {invalidName != null && (
                  <p
                    className="text-body text-ember-300"
                    data-testid="clone-name-error"
                  >
                    {invalidName}
                  </p>
                )}
                <div className="flex flex-wrap gap-2">
                  <button
                    type="button"
                    data-testid="clone-preview"
                    onClick={() => void preview()}
                    disabled={state.busy}
                    className="rounded-lg border border-ink-800 px-3 py-2 text-body text-ink-200 disabled:opacity-50"
                  >
                    {t("voice.clone.hearIt")}
                  </button>
                  <button
                    type="button"
                    data-testid="clone-retake"
                    onClick={() => void discard()}
                    disabled={state.busy}
                    className="rounded-lg border border-ink-800 px-3 py-2 text-body text-ink-200 disabled:opacity-50"
                  >
                    {t("voice.clone.startOver")}
                  </button>
                  <button
                    type="button"
                    data-testid="clone-save"
                    onClick={() => void save()}
                    disabled={!canSave(state) || invalidName != null}
                    className="rounded-lg bg-ink-50 px-3 py-2 text-body font-medium text-ink-950 transition-colors hover:bg-ink-150 disabled:opacity-50"
                  >
                    {t("voice.clone.save")}
                  </button>
                </div>
              </div>
            )}

            {state.error != null && (
              <p className="text-body text-ember-300" data-testid="clone-error">
                {t(state.errorKey ?? "voice.clone.err.generic")}
              </p>
            )}

            <div className="space-y-1 border-t border-ink-800 pt-3">
              <p
                className="text-secondary text-ink-500"
                data-testid="clone-consent"
              >
                {t("voice.clone.consentNote")}
              </p>
              <p
                className="text-secondary text-ink-500"
                data-testid="clone-watermark"
              >
                {t("voice.clone.watermarkNote")}
              </p>
            </div>
          </>
        )}
      </div>
    </div>
  );
}
