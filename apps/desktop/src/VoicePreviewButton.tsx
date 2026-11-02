import { useCallback, useEffect, useRef, useState } from "react";
import { invoke } from "@tauri-apps/api/core";

import { engineRefusal } from "./engineSetupModel";
import { openEngineSetup } from "./engineSetupStore";
import { getMediaUrl } from "./mediaUrl";
import { useI18n } from "./i18n";

export type PreviewState = "idle" | "loading" | "playing" | "error";

export function VoicePreviewButton({
  voiceProfileId,
  language,
  disabled,
}: {
  voiceProfileId: string;
  language?: string | null;
  disabled?: boolean;
}) {
  const { t } = useI18n();
  const [state, setState] = useState<PreviewState>("idle");
  const audioRef = useRef<HTMLAudioElement | null>(null);
  const activeRef = useRef(true);
  const voiceRef = useRef(voiceProfileId);

  useEffect(() => {
    activeRef.current = true;
    return () => {
      activeRef.current = false;
      audioRef.current?.pause();
      audioRef.current = null;
    };
  }, []);

  useEffect(() => {
    voiceRef.current = voiceProfileId;
    setState("idle");
    audioRef.current?.pause();
    audioRef.current = null;
  }, [voiceProfileId]);

  const stop = useCallback(() => {
    audioRef.current?.pause();
    audioRef.current = null;
    setState("idle");
  }, []);

  const play = useCallback(async () => {
    if (state === "playing") {
      stop();
      return;
    }
    setState("loading");
    try {
      const wavPath = await invoke<string>("preview_voice", {
        voiceProfileId,
        language: language ?? null,
      });
      const url = await getMediaUrl(wavPath);
      if (!activeRef.current || voiceRef.current !== voiceProfileId) return;
      const audio = new Audio(url);
      audio.onended = () => {
        if (activeRef.current && voiceRef.current === voiceProfileId) {
          setState("idle");
        }
      };
      audio.onerror = () => {
        if (activeRef.current && voiceRef.current === voiceProfileId) {
          setState("error");
        }
      };
      audioRef.current = audio;
      await audio.play();
      if (activeRef.current && voiceRef.current === voiceProfileId) {
        setState("playing");
      }
    } catch (raw) {
      if (activeRef.current && voiceRef.current === voiceProfileId) {
        const need = engineRefusal(String(raw));
        if (need != null) {
          setState("idle");
          openEngineSetup(need);
        } else {
          setState("error");
        }
      }
    }
  }, [state, stop, voiceProfileId, language]);

  const label =
    state === "loading"
      ? t("voice.preview.loading")
      : state === "playing"
        ? t("voice.preview.playing")
        : state === "error"
          ? t("voice.preview.error")
          : t("voice.preview.idle");

  return (
    <button
      type="button"
      data-testid="voice-preview"
      data-state={state}
      aria-label={label}
      disabled={disabled || state === "loading"}
      onClick={() => void play()}
      className="flex items-center gap-1.5 rounded-lg border border-ink-700 bg-ink-800 px-2.5 py-1.5 text-secondary font-medium text-ink-200 transition-colors hover:bg-ink-700 active:scale-[0.97] disabled:cursor-not-allowed disabled:opacity-50"
    >
      {state === "loading" ? (
        <span className="shimmer-bar inline-block h-3 w-3 rounded-full bg-ink-600" />
      ) : state === "playing" ? (
        <span aria-hidden>◼</span>
      ) : (
        <span aria-hidden>▶</span>
      )}
      {state === "error"
        ? t("voice.preview.retry")
        : state === "playing"
          ? t("voice.preview.stop")
          : t("voice.preview.label")}
    </button>
  );
}

export default VoicePreviewButton;
