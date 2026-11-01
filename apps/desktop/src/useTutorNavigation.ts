import { useCallback, useEffect, useRef, useState } from "react";
import { convertFileSrc, invoke } from "@tauri-apps/api/core";
import { navigationPageToApply } from "./viewportSyncModel";
import type { ReadingPage } from "./readingModel";
import type { VoiceNavigation } from "./voiceSessionModel";
import type { NavigationCheckpoint } from "./readerCheckpoint";

interface NavigationOutcome {
  callId: string;
  applied: boolean;
  held: boolean;
}
interface Inputs {
  checkpoint?: NavigationCheckpoint;
  onCheckpoint?: (value: NavigationCheckpoint) => void;
  sessionId: string | null;
  bookId: string;
  live: boolean;
  navigation: VoiceNavigation | null;
  outcome: NavigationOutcome | null;
  pages: Map<number, ReadingPage>;
  shownPage: number | null;
  fetchPages: (pages: number[]) => void;
  apply: (navigation: VoiceNavigation) => void;
  freeze: () => void;
}

type Disposition = "applied" | "held" | "unavailable" | "superseded" | "resume";

interface Pending {
  sessionId: string | null;
  acknowledged: boolean;
  resuming: boolean;
  navigation: VoiceNavigation;
  previous: number | null;
  displayed: boolean;
}

export function useTutorNavigation(inputs: Inputs) {
  const current = useRef(inputs);
  current.current = inputs;
  const [held, setHeld] = useState(inputs.checkpoint?.held ?? false);
  const heldRef = useRef(held);
  const [latest, setLatest] = useState<VoiceNavigation | null>(
    inputs.checkpoint?.latest ?? null,
  );
  const [pending, setPending] = useState<Pending | null>(null);
  const pendingRef = useRef<Pending | null>(null);
  const consumed = useRef(inputs.checkpoint?.consumed ?? 0);
  const scope = `${inputs.bookId}:${inputs.sessionId ?? ""}`;

  const acknowledge = useCallback(
    (
      navigation: VoiceNavigation,
      disposition: Disposition,
      sessionId = current.current.sessionId,
    ) => {
      if (
        navigation.callId == null ||
        navigation.generation == null ||
        navigation.pageNo == null
      )
        return;
      void invoke("voice_session_navigation_ack", {
        sessionId,
        callId: navigation.callId,
        generation: navigation.generation,
        pageNo: navigation.pageNo,
        disposition,
      }).catch(console.error);
    },
    [],
  );

  const begin = useCallback(
    (navigation: VoiceNavigation, resuming = false) => {
      if (navigation.pageNo == null) return;
      if (pendingRef.current != null)
        acknowledge(
          pendingRef.current.navigation,
          "superseded",
          pendingRef.current.sessionId,
        );
      const next = {
        sessionId: current.current.sessionId,
        acknowledged: false,
        resuming,
        navigation,
        previous: current.current.shownPage,
        displayed: false,
      };
      pendingRef.current = next;
      setPending(next);
      current.current.fetchPages([navigation.pageNo]);
    },
    [acknowledge],
  );

  useEffect(() => {
    const saved = current.current.checkpoint;
    consumed.current = saved?.consumed ?? 0;
    heldRef.current = saved?.held ?? false;
    setHeld(heldRef.current);
    setLatest(saved?.latest ?? null);
    return () => {
      if (pendingRef.current != null)
        acknowledge(
          pendingRef.current.navigation,
          "unavailable",
          pendingRef.current.sessionId,
        );
      pendingRef.current = null;
    };
  }, [scope, acknowledge]);

  useEffect(() => {
    const navigation = inputs.navigation;
    if (
      !inputs.live ||
      navigation == null ||
      navigation.nonce <= consumed.current
    )
      return;
    if (
      navigation.callId != null &&
      inputs.outcome?.callId === navigation.callId
    ) {
      consumed.current = navigation.nonce;
      setLatest(
        inputs.outcome.applied || inputs.outcome.held ? navigation : null,
      );
      return;
    }
    const pageToApply = navigationPageToApply(
      navigation,
      consumed.current,
      heldRef.current,
    );
    consumed.current = navigation.nonce;
    setLatest(navigation);
    if (pageToApply == null) {
      acknowledge(navigation, "held");
    } else if (navigation.callId == null) {
      current.current.apply(navigation);
    } else {
      begin(navigation);
    }
  }, [
    inputs.live,
    inputs.navigation,
    inputs.outcome,
    scope,
    acknowledge,
    begin,
  ]);

  useEffect(() => {
    current.current.onCheckpoint?.({
      consumed: consumed.current,
      held,
      latest,
    });
  }, [inputs.onCheckpoint, inputs.navigation, held, latest]);

  const page =
    pending?.navigation.pageNo == null
      ? null
      : inputs.pages.get(pending.navigation.pageNo);
  const path = page?.imagePath;
  useEffect(() => {
    if (pending == null || path == null || pending.displayed) return;
    let active = true;
    const image = new Image();
    image.onload = () => {
      if (!active || pendingRef.current !== pending) return;
      const displayed = { ...pending, displayed: true };
      pendingRef.current = displayed;
      setPending(displayed);
      current.current.apply(pending.navigation);
    };
    image.onerror = () => {
      if (!active || pendingRef.current !== pending) return;
      acknowledge(pending.navigation, "unavailable", pending.sessionId);
      pendingRef.current = null;
      setPending(null);
    };
    image.src = convertFileSrc(path);
    return () => {
      active = false;
      image.onload = null;
      image.onerror = null;
    };
  }, [path, pending, acknowledge]);

  const pageReady = useCallback(
    (pageNo: number) => {
      const waiting = pendingRef.current;
      if (
        waiting?.displayed &&
        !waiting.acknowledged &&
        waiting.navigation.pageNo === pageNo
      ) {
        waiting.acknowledged = true;
        acknowledge(
          waiting.navigation,
          waiting.resuming ? "resume" : "applied",
          waiting.sessionId,
        );
      }
    },
    [acknowledge],
  );

  useEffect(() => {
    const outcome = inputs.outcome;
    if (outcome != null && !outcome.applied && !outcome.held)
      setLatest((value) => (value?.callId === outcome.callId ? null : value));
    const waiting = pendingRef.current;
    if (
      waiting == null ||
      outcome == null ||
      outcome.callId !== waiting.navigation.callId
    )
      return;
    if (
      !outcome.applied &&
      !outcome.held &&
      waiting.displayed &&
      waiting.previous != null
    ) {
      current.current.apply({
        ...waiting.navigation,
        callId: undefined,
        pageNo: waiting.previous,
      });
    }
    pendingRef.current = null;
    setPending(null);
  }, [inputs.outcome]);

  useEffect(() => {
    if (inputs.live) return;
    if (pendingRef.current != null)
      acknowledge(
        pendingRef.current.navigation,
        "unavailable",
        pendingRef.current.sessionId,
      );
    pendingRef.current = null;
    setPending(null);
    setLatest(null);
    heldRef.current = false;
    setHeld(false);
  }, [inputs.live, acknowledge]);

  const keepPage = useCallback(() => {
    heldRef.current = true;
    setHeld(true);
    current.current.freeze();
    if (pendingRef.current != null) {
      acknowledge(pendingRef.current.navigation, "held");
      pendingRef.current = null;
      setPending(null);
    }
  }, [acknowledge]);

  const goToTutor = useCallback(() => {
    heldRef.current = false;
    setHeld(false);
    if (latest != null) {
      if (latest.callId == null) current.current.apply(latest);
      else begin(latest, true);
    }
  }, [latest, begin]);

  return {
    held,
    latest,
    keepPage,
    goToTutor,
    pageReady,
    readyToken:
      pending == null
        ? ""
        : `${pending.navigation.callId}:${pending.displayed}`,
  };
}
