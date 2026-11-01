// The Book tab of the Study companion (charter §7, B-4): calm entry into the
// whole-book Agent — not a provider console. Probe-backed states, the
// whole-book/spoiler disclosure, the explicit consent card, the explicit
// switch confirmation when the Tutor is live, and the live session
// passthrough (the shared session surface renders the conversation).

import { useCallback, useEffect, useState } from "react";
import { invoke } from "@tauri-apps/api/core";

import {
  bookAgentStateFrom,
  categoryLabel,
  type BookAgentState,
} from "./bookAgentModel";
import { useI18n } from "./i18n";
import Button from "./ui/Button";

function BookAgentPanel({
  bookId,
  jobId,
  playbackPosMs,
  sessionLive,
  session,
  fallback,
}: {
  bookId: string;
  jobId: string | null;
  playbackPosMs: () => number;
  /** ANY live voice session exists (the store does not know its mode). */
  sessionLive: boolean;
  session: React.ReactNode;
  /** Rendered when the Rust build has no Book Agent (feature-off): the calm
   * book-metadata panel this tab showed before. */
  fallback: React.ReactNode;
}) {
  const { t, tx } = useI18n();
  const [state, setState] = useState<BookAgentState>({ kind: "loading" });
  const [confirming, setConfirming] = useState(false);
  // Whether the live session is OURS (started from this panel). The Rust
  // slot + mode tag own the authority law; this only picks the UI.
  const [started, setStarted] = useState(false);
  const live = started && sessionLive;
  const tutorLive = sessionLive && !started;
  useEffect(() => {
    if (!sessionLive && started) setStarted(false);
  }, [sessionLive, started]);

  const probe = useCallback(async () => {
    try {
      const answer = await invoke("agent_session_probe", { bookId });
      const next = bookAgentStateFrom(answer);
      // Runtime truth for a live session's mode (§7.3): never guessed.
      if (next.kind === "liveAgent") setStarted(true);
      if (next.kind === "liveTutor") setStarted(false);
      setState(next);
    } catch (error) {
      // A feature-off Rust build has no agent commands: this tab stays the
      // calm metadata panel, never a raw error card.
      const message = String(error);
      if (message.includes("not found") || message.includes("unknown")) {
        setState({ kind: "notInBuild" });
      } else {
        setState({ kind: "failed", message });
      }
    }
  }, [bookId]);

  useEffect(() => {
    if (!live) void probe();
  }, [probe, live]);

  const start = useCallback(
    async (confirmed: boolean) => {
      if (jobId == null) {
        setState({ kind: "failed", message: "no playable render" });
        return;
      }
      setState({ kind: "starting" });
      try {
        await invoke("agent_session_start", {
          bookId,
          jobId,
          playbackPosMs: playbackPosMs(),
          confirmed,
        });
        setStarted(true);
        setState({ kind: "live" });
      } catch (error) {
        setState({ kind: "failed", message: String(error) });
      }
    },
    [bookId, jobId, playbackPosMs],
  );

  if (state.kind === "notInBuild") {
    return <>{fallback}</>;
  }
  if (
    live ||
    state.kind === "live" ||
    state.kind === "liveAgent" ||
    state.kind === "liveTutor"
  ) {
    return <>{session}</>;
  }

  return (
    <div
      data-testid="book-agent-panel"
      className="flex h-full w-full shrink-0 flex-col gap-3 border-s border-ink-800 bg-ink-900 px-4 py-4"
    >
      <p className="font-serif text-heading leading-snug font-semibold text-ink-50">
        {t("tutor.bookAgent")}
      </p>
      <p className="text-secondary text-ink-300">
        {t("tutor.wholeBookDisclosure")}
      </p>

      {state.kind === "loading" && (
        <p
          data-testid="book-agent-loading"
          className="text-secondary text-ink-400"
        >
          {t("tutor.checking")}
        </p>
      )}

      {state.kind === "unsupported" && (
        <p
          data-testid="book-agent-unsupported"
          className="text-secondary text-ink-400"
        >
          {t("tutor.unsupported")}
        </p>
      )}

      {state.kind === "failed" && (
        <div data-testid="book-agent-failed" className="flex flex-col gap-2">
          <p className="text-secondary text-ink-300">
            {tx("tutor.failed", { message: state.message })}
          </p>
          <Button
            variant="soft"
            size="sm"
            className="self-start"
            onClick={() => void probe()}
          >
            {t("tutor.tryAgain")}
          </Button>
        </div>
      )}

      {state.kind === "consentRequired" && (
        <div data-testid="book-agent-consent" className="flex flex-col gap-2">
          <p className="text-secondary text-ink-200">
            {t("tutor.consentIntro")}
          </p>
          <ul className="flex flex-col gap-1 text-secondary text-ink-300">
            {state.hosts.map((host) => (
              <li
                key={host}
                data-testid="book-agent-consent-host"
                dir="ltr"
                className="font-mono text-label"
              >
                {host}
              </li>
            ))}
          </ul>
          <p className="text-secondary text-ink-200">
            {t("tutor.consentSends")}
          </p>
          <ul className="list-disc ps-5 text-secondary text-ink-300">
            {state.dataCategories.map((category) => (
              <li key={category}>{categoryLabel(category)}</li>
            ))}
          </ul>
          {state.retentionUrl != null && (
            <a
              href={state.retentionUrl}
              target="_blank"
              rel="noreferrer"
              className="text-secondary text-ink-400 underline"
            >
              {t("tutor.dataPolicy")}
            </a>
          )}
          <Button
            variant="solid"
            size="sm"
            className="self-start"
            data-testid="book-agent-consent-grant"
            onClick={() => {
              void invoke("agent_consent_grant", {
                bookId,
                connectionId: state.connectionId,
                shownHosts: state.hosts,
              })
                .then(() => probe())
                .catch((error) =>
                  setState({ kind: "failed", message: String(error) }),
                );
            }}
          >
            {t("tutor.allowForBook")}
          </Button>
        </div>
      )}

      {state.kind === "ready" && !confirming && (
        <Button
          variant="solid"
          size="sm"
          className="self-start"
          data-testid="book-agent-start"
          onClick={() => {
            if (tutorLive) {
              setConfirming(true);
            } else {
              void start(false);
            }
          }}
        >
          {t("tutor.startAgent")}
        </Button>
      )}

      {state.kind === "ready" && confirming && (
        <div
          data-testid="book-agent-switch-confirm"
          role="alertdialog"
          aria-label={t("tutor.switchAria")}
          className="flex flex-col gap-2 rounded-lg border border-ink-700 bg-ink-800/60 p-3"
        >
          <p className="text-secondary text-ink-200">
            {t("tutor.switchConfirmation")}
          </p>
          <div className="flex gap-2">
            <Button
              variant="solid"
              size="sm"
              data-testid="book-agent-switch-yes"
              onClick={() => void start(true)}
            >
              {t("tutor.switchYes")}
            </Button>
            <Button
              size="sm"
              data-testid="book-agent-switch-no"
              onClick={() => setConfirming(false)}
            >
              {t("tutor.switchNo")}
            </Button>
          </div>
        </div>
      )}

      {state.kind === "starting" && (
        <p
          data-testid="book-agent-starting"
          className="text-secondary text-ink-400"
        >
          {t("tutor.preparing")}
        </p>
      )}
    </div>
  );
}

export default BookAgentPanel;
