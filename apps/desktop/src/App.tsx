import {
  useCallback,
  useEffect,
  useMemo,
  useRef,
  useState,
  useSyncExternalStore,
} from "react";
import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";

import AppNavigationControls from "./AppNavigationControls";
import {
  destinationKey,
  pushDestination,
  type AppDestination,
  type AppHistory,
} from "./appNavigation";
import type { ReaderCheckpoint } from "./readerCheckpoint";
import { ensureHalfDuplexGate } from "./voiceHalfDuplex";
import JobsPanel from "./JobsPanel";
import EngineSetupSheet from "./EngineSetupSheet";
import Library, { filterSegment, type ShelfFilter } from "./Library";
import CastDraftReturn from "./CastDraftReturn";
import ModelsPanel from "./ModelsPanel";
import ProvidersPage from "./ProvidersPage";
import Settings from "./Settings";
import SettingsFrame, { isSettingsSection } from "./SettingsFrame";
import ReportIssueModal from "./ReportIssueModal";
import { ShellActivityPill, useEngineStatus } from "./ShellActivity";
import ShellRail from "./ShellRail";
import CommandPalette from "./CommandPalette";
import ActivityTray, { useStudioJobs } from "./ActivityTray";
import { AppUpdateBanner } from "./AppUpdateBanner";
import TutorLauncher from "./TutorLauncher";
import TutorPanel from "./TutorPanel";
import { loadDock, storeDock, type TutorDock } from "./tutorDockModel";
import PlaybackEngine from "./PlaybackEngine";
import SleepTimerDriver from "./SleepTimerDriver";
import ReadingView from "./ReadingView";
import MiniPlayer from "./MiniPlayer";
import LaunchIntro from "./LaunchIntro";
import { SettingsIcon } from "./Icons";
import Wizard from "./Wizard";
import { reportLayout, reportLayoutWhenReady } from "./layoutDiagnostics";
import { initTheme } from "./themeStore";
import { startVoicePlaybackBridge } from "./voicePlaybackBridge";
import { useZoom } from "./useZoom";
import { buildSetupChecklist, type SetupChecklist } from "./setupChecklist";
import { LITE_VARIANT } from "./engineSetupModel";
import {
  installedSet,
  type FullCatalog,
  type InstalledModel,
} from "./modelCatalogModel";
import {
  dispatch,
  getSnapshot as getPlaybackSnapshot,
  subscribe as subscribePlayback,
} from "./playbackStore";
import {
  freshVoicePanelForBook,
  getActiveVoiceSnapshot,
  synchronizeVoiceSession,
  sessionIsActive,
  subscribeVoice,
  type ActiveVoiceSession,
} from "./voiceSessionStore";
import { usePlayback } from "./usePlayback";
import {
  canFollowAlong,
  overlayState,
  type OverlayAction,
  type OverlayState,
} from "./readingOverlayModel";
import {
  buildAudioShelf,
  type AudioShelfItem,
  type ExportSummary,
} from "./audioShelfModel";
import { bookDisplayTitle, type BookSummary } from "./libraryModel";
import {
  loadLastPlace,
  placeOf,
  resolveLastPlace,
  saveLastPlace,
} from "./lastPlaceModel";
import { bookAvailability } from "./libraryAvailability";
import { listenedOf, loadListened } from "./listeningProgress";
import { isTerminal, type JobSummary } from "./jobModel";
import { tutorEntryRoute } from "./tutorEntryModel";
import { isBookSurface, type BookSurfaceItem } from "./viewportSyncModel";
import {
  BOOK_OPEN_EVENT,
  BOOK_TUTOR_EVENT,
  GO_VIEW_EVENT,
  OPEN_TUTOR_EVENT,
  RETURN_TUTOR_EVENT,
  REPORT_ISSUE_EVENT,
  SETUP_CHANGED_EVENT,
  goToAudioShelf,
  goToCastBook,
  goToView,
  openTutor,
  requestBookOpen,
  showReaderTutor,
  returnToTutor,
  type View,
} from "./views";
import type { PaletteTarget } from "./commandPaletteModel";
import { useI18n, type MessageKey } from "./i18n";

function App() {
  const { t } = useI18n();
  const activeVoice = useSyncExternalStore(
    subscribeVoice,
    getActiveVoiceSnapshot,
  );
  const [returnError, setReturnError] = useState<MessageKey | null>(null);
  const [returning, setReturning] = useState(false);
  const [activeTitle, setActiveTitle] = useState("");
  const activeBookId =
    activeVoice.identity?.bookId ?? activeVoice.pendingTarget?.bookId;
  useEffect(() => {
    setActiveTitle("");
    if (activeBookId == null) return;
    let current = true;
    void invoke<BookSummary[]>("list_books")
      .then((books) => {
        const book = books.find((entry) => entry.id === activeBookId);
        if (current && book != null) setActiveTitle(bookDisplayTitle(book));
      })
      .catch(() => {});
    return () => {
      current = false;
    };
  }, [activeBookId]);
  const checkpoints = useRef(new Map<string, ReaderCheckpoint>());
  const [version, setVersion] = useState<string>("");
  // A `#providers/<id>` launch address is a REAL deep link (S-5 IA gate):
  // the app mounts straight into the Providers view and the page itself
  // resolves the addressed provider detail from the same hash.
  const [view, setView] = useState<View>(() =>
    /^#providers(\/|$)/.test(window.location.hash) ? "providers" : "library",
  );
  const [checklist, setChecklist] = useState<SetupChecklist | null>(null);
  const [overlay, setOverlay] = useState<OverlayState>("none");
  const [onboarded, setOnboarded] = useState<boolean | null>(null);
  const playbackItem = usePlayback((s) => s.item);
  const hasPlaybackItem = playbackItem !== null;
  const [trayOpen, setTrayOpen] = useState(false);
  const [reportIssueOpen, setReportIssueOpen] = useState(false);
  const [paletteOpen, setPaletteOpen] = useState(false);

  useEffect(() => {
    const onKeyDown = (event: KeyboardEvent) => {
      if (
        !(event.ctrlKey || event.metaKey) ||
        event.altKey ||
        event.shiftKey ||
        event.key.toLowerCase() !== "k"
      ) {
        return;
      }
      const target = event.target as HTMLElement | null;
      const inPalette =
        target?.closest('[data-testid="command-palette"]') != null;
      const editing =
        target != null &&
        (target.tagName === "INPUT" ||
          target.tagName === "TEXTAREA" ||
          target.isContentEditable);
      if (editing && !inPalette) return;
      const blockingDialog = Array.from(
        document.querySelectorAll<HTMLElement>('[aria-modal="true"]'),
      ).some(
        (dialog) =>
          dialog.closest('[data-testid="command-palette"]') == null &&
          dialog.closest('[data-testid="activity-tray"]') == null &&
          dialog.closest('[data-testid="reading-view"]') == null,
      );
      if (blockingDialog) return;
      event.preventDefault();
      setTrayOpen(false);
      setPaletteOpen((open) => !open);
    };
    window.addEventListener("keydown", onKeyDown);
    return () => window.removeEventListener("keydown", onKeyDown);
  }, []);

  const choosePaletteTarget = (target: PaletteTarget) => {
    switch (target.kind) {
      case "book": {
        const followable =
          target.playable != null && canFollowAlong(target.playable)
            ? target.playable
            : null;
        const study = followable ?? (target.hasPages ? null : target.playable);
        if (study != null) {
          openSession(study, false, listenedOf(loadListened(), study.id));
        } else if (target.hasPages) {
          requestBookOpen(target.bookId);
        }
        return;
      }
      case "cast":
        if (overlay === "reading") {
          setBookSurface(null);
          applyOverlay({ type: "closeReading" });
        }
        dispatch({ type: "load", item: target.item, autoplay: true });
        return;
      case "chapter":
        dispatch({ type: "seekMs", ms: target.startMs });
        dispatch({ type: "play" });
        return;
      case "view":
        setTrayOpen(false);
        goToView(target.view);
        return;
      case "action":
        if (target.action === "tutor") {
          if (overlay === "reading") showReaderTutor();
          else openTutor();
        } else if (target.action === "report") setReportIssueOpen(true);
        else setTrayOpen(true);
        return;
    }
  };
  const [tutorPanel, setTutorPanel] = useState<number | null>(null);
  const [tutorDock, setTutorDock] = useState<TutorDock>(loadDock);
  const [dockOverlay, setDockOverlay] = useState(overlay);
  if (dockOverlay !== overlay) {
    setDockOverlay(overlay);
    if (overlay === "none") setTutorDock(loadDock());
  }
  const applyTutorDock = useCallback((next: TutorDock) => {
    setTutorDock(next);
    storeDock(next);
  }, []);
  const [bootPlace] = useState(loadLastPlace);
  const [audioCheck, setAudioCheck] = useState<{
    surface: BookSurfaceItem;
    noAudio: boolean;
  } | null>(null);
  const engine = useEngineStatus();
  const studio = useStudioJobs();
  const [shelfFilter, setShelfFilter] = useState<ShelfFilter>("all");
  const castingBookIds = useMemo(
    () =>
      new Set(
        (studio.jobs ?? [])
          .filter((job) => !isTerminal(job.state))
          .map((job) => job.bookId),
      ),
    [studio.jobs],
  );
  useZoom();

  useEffect(() => {
    const handleReportIssue = () => setReportIssueOpen(true);
    window.addEventListener(REPORT_ISSUE_EVENT, handleReportIssue);
    return () =>
      window.removeEventListener(REPORT_ISSUE_EVENT, handleReportIssue);
  }, []);

  useEffect(() => {
    const handleOpenTutor = () => {
      setTrayOpen(false);
      setTutorDock((current) => {
        if (current.mode !== "floating" || !current.folded) return current;
        const next = { ...current, folded: false };
        storeDock(next);
        return next;
      });
      setTutorPanel((nonce) => (nonce ?? 0) + 1);
    };
    window.addEventListener(OPEN_TUTOR_EVENT, handleOpenTutor);
    return () => window.removeEventListener(OPEN_TUTOR_EVENT, handleOpenTutor);
  }, []);

  const closeTutorPanel = useCallback(() => setTutorPanel(null), []);
  useEffect(() => {
    ensureHalfDuplexGate();
    void synchronizeVoiceSession().catch(console.error);
  }, []);

  useEffect(() => initTheme(), []);

  useEffect(() => startVoicePlaybackBridge(), []);

  useEffect(() => {
    void reportLayoutWhenReady("mount");
    const onResize = () => void reportLayout("resize");
    window.addEventListener("resize", onResize);
    return () => {
      window.removeEventListener("resize", onResize);
    };
  }, []);

  const returnFocusRef = useRef<string | null>(null);

  const applyOverlay = useCallback((action: OverlayAction) => {
    if (action.type === "openReading") {
      const active = document.activeElement as HTMLElement | null;
      returnFocusRef.current = active?.dataset?.testid ?? null;
      setTutorPanel(null);
    }
    setOverlay((current) => overlayState(current, action));
  }, []);

  // F8 D2: the job-independent BookSurface reader (a BookDirect voice
  // session's viewport). While it is mounted the reading overlay does not
  // depend on a playback item at all.
  const [bookSurface, setBookSurface] = useState<BookSurfaceItem | null>(null);
  const readingItem = bookSurface ?? playbackItem;
  const parkedReader = useRef<AudioShelfItem | BookSurfaceItem | null>(null);
  if (overlay === "reading" && readingItem != null)
    parkedReader.current = readingItem;
  const [history, setHistory] = useState<AppHistory>(() => ({
    entries: [{ kind: "view", view }],
    index: 0,
  }));
  const historyTarget = useRef<string | null>(null);
  const returnRequest = useRef(0);
  const checkpointKey = (item: AudioShelfItem | BookSurfaceItem) =>
    destinationKey({ kind: "reader", place: placeOf(item) });
  useEffect(() => {
    const destination: AppDestination =
      overlay === "reading" && readingItem != null
        ? { kind: "reader", place: placeOf(readingItem) }
        : { kind: "view", view };
    const key = destinationKey(destination);
    if (historyTarget.current === key) {
      historyTarget.current = null;
      return;
    }
    historyTarget.current = null;
    setHistory((current) => pushDestination(current, destination));
  }, [view, overlay, readingItem]);

  useEffect(() => {
    if (playbackItem == null && bookSurface == null) {
      applyOverlay({ type: "stop" });
    }
  }, [applyOverlay, bookSurface, playbackItem]);

  useEffect(() => {
    if (overlay !== "none") return;
    const testid = returnFocusRef.current;
    returnFocusRef.current = null;
    if (testid == null) return;
    const frame = requestAnimationFrame(() => {
      const target = document.querySelector<HTMLElement>(
        `[data-testid="${testid}"]`,
      );
      target?.focus();
    });
    return () => cancelAnimationFrame(frame);
  }, [overlay]);

  const playInStrip = useCallback((item: AudioShelfItem, autoplay: boolean) => {
    dispatch({ type: "load", item, autoplay });
  }, []);

  const [resumeNotice, setResumeNotice] = useState(false);
  const pendingSeekRef = useRef<{ id: string; fraction: number } | null>(null);
  const audioPlaces = useRef(
    new Map<string, { fraction: number; jobId: string | null }>(),
  );
  useEffect(
    () =>
      subscribePlayback(() => {
        const current = getPlaybackSnapshot();
        if (current.item == null || !(current.duration > 0)) return;
        const id = current.item.id;
        audioPlaces.current.delete(id);
        audioPlaces.current.set(id, {
          fraction: current.current / current.duration,
          jobId: current.item.jobId,
        });
        if (audioPlaces.current.size > 20) {
          const activeJob = getActiveVoiceSnapshot().identity?.jobId;
          for (const [key, value] of audioPlaces.current) {
            if (activeJob == null || value.jobId !== activeJob) {
              audioPlaces.current.delete(key);
              break;
            }
          }
          if (audioPlaces.current.size > 20)
            audioPlaces.current.delete(
              audioPlaces.current.keys().next().value!,
            );
        }
      }),
    [],
  );
  const [sessionNonce, setSessionNonce] = useState(0);
  const playbackDuration = usePlayback((s) => s.duration);
  const playbackPlaying = usePlayback((s) => s.playing);

  useEffect(() => {
    if (playbackPlaying) setResumeNotice(false);
  }, [playbackPlaying]);

  const openSession = useCallback(
    (
      item: AudioShelfItem,
      resumed: boolean,
      fraction: number | null,
      // Play intent (F7/F8): true for Study/player entries where the user
      // pressed play; false for Talk entries, which mount the reader around
      // a live tutor — the item loads ready-but-strictly-paused, and no
      // `playing: true` state ever exists for the engine to start from.
      autoplay = true,
    ) => {
      if (!canFollowAlong(item)) {
        playInStrip(item, autoplay && !resumed);
        return;
      }
      // Every open REPLACES the parked seek: an earlier resume still waiting
      // for metadata must not drag a later open (e.g. a fresh Talk session
      // at 0:00) back to the old listening spot once the duration arrives.
      pendingSeekRef.current =
        fraction != null && fraction > 0 && fraction < 1
          ? { id: item.id, fraction }
          : null;
      setBookSurface(null);
      dispatch({ type: "load", item, autoplay: autoplay && !resumed });
      // Resume keeps its pre-existing contract: land paused even when the
      // item was already loaded (the neutral load above leaves `playing`
      // untouched on a same-item load).
      if (resumed) dispatch({ type: "pause" });
      setSessionNonce((nonce) => nonce + 1);
      setResumeNotice(resumed);
      applyOverlay({ type: "openReading" });
    },
    [applyOverlay, playInStrip],
  );

  useEffect(() => {
    const pending = pendingSeekRef.current;
    if (pending == null) return;
    if (playbackItem?.id !== pending.id || !(playbackDuration > 0)) return;
    pendingSeekRef.current = null;
    dispatch({ type: "seekFraction", fraction: pending.fraction });
  }, [playbackDuration, playbackItem, sessionNonce]);

  // Talk entry (Library, JobModal): a fresh tutor session with the book
  // itself — its pages via the BookSurface reader whenever they render, a
  // readable cast only for books without page renders, and the Study card
  // latch as the no-render fallback (tutorEntryRoute, owner decision
  // 2026-10-09). The previous, finished conversation never greets it.
  const placeMovesRef = useRef(0);
  const playingId = playbackItem?.id ?? null;
  useEffect(() => {
    placeMovesRef.current += 1;
  }, [bookSurface, overlay, playingId, tutorPanel, view]);

  useEffect(() => {
    const onBookTutor = (event: Event) => {
      const bookId = (event as CustomEvent<string>).detail;
      if (!bookId) return;
      const active = getActiveVoiceSnapshot();
      if (sessionIsActive(active) && active.identity?.bookId === bookId) {
        returnToTutor();
        return;
      }
      const movesAtStart = placeMovesRef.current;
      Promise.all([
        invoke<ExportSummary[]>("list_exports"),
        invoke<BookSummary[]>("list_books"),
      ])
        .then(([exports, books]) => {
          if (placeMovesRef.current !== movesAtStart) return;
          const book = books.find((entry) => entry.id === bookId) ?? null;
          const route = tutorEntryRoute(book, buildAudioShelf(exports, books));
          freshVoicePanelForBook(bookId);
          if (route.kind === "reader") {
            // Talk entry: mount the reader playback-neutral. Narration is a
            // user action — transport visible but idle until the user plays
            // or asks the tutor to navigate. A fresh session starts at the
            // beginning, not wherever the user last stopped listening.
            openSession(route.item, false, null, false);
            dispatch({ type: "seekFraction", fraction: 0 });
          } else if (route.kind === "bookSurface") {
            setBookSurface(route.surface);
            applyOverlay({ type: "openReading" });
          } else {
            openTutor(bookId);
          }
        })
        .catch(console.error);
    };
    window.addEventListener(BOOK_TUTOR_EVENT, onBookTutor);
    return () => window.removeEventListener(BOOK_TUTOR_EVENT, onBookTutor);
  }, [applyOverlay, openSession]);

  const restoreDestination = useCallback(
    async (destination: AppDestination, active?: ActiveVoiceSession) => {
      if (destination.kind === "view") {
        goToView(destination.view);
        return true;
      }
      const moves = placeMovesRef.current;
      const request = ++returnRequest.current;
      setReturning(true);
      setReturnError(null);
      try {
        const [books, exports] = await Promise.all([
          invoke<BookSummary[]>("list_books"),
          invoke<ExportSummary[]>("list_exports"),
        ]);
        if (
          moves !== placeMovesRef.current ||
          request !== returnRequest.current
        )
          return false;
        if (active != null) {
          const now = getActiveVoiceSnapshot();
          if (
            !sessionIsActive(now) ||
            now.identity?.sessionId !== active.sessionId
          )
            return false;
        }
        const shelf = buildAudioShelf(exports, books);
        const place = destination.place;
        const resolved = resolveLastPlace(place, books, shelf);
        if (
          resolved == null ||
          (place.kind === "cast" &&
            (resolved.kind !== "cast" || resolved.item.id !== place.exportId))
        )
          throw new Error("unavailable");
        if (
          active != null &&
          resolved.kind === "cast" &&
          resolved.item.jobId !== active.jobId
        )
          throw new Error("unavailable");
        if (resolved.kind === "pages") setBookSurface(resolved.surface);
        else {
          setBookSurface(null);
          const current = getPlaybackSnapshot().item;
          const saved = audioPlaces.current.get(resolved.item.id);
          pendingSeekRef.current =
            current?.id !== resolved.item.id &&
            saved != null &&
            saved.fraction > 0 &&
            saved.fraction < 1
              ? { id: resolved.item.id, fraction: saved.fraction }
              : null;
          dispatch({ type: "load", item: resolved.item, autoplay: false });
        }
        setTrayOpen(false);
        setResumeNotice(false);
        applyOverlay({ type: "openReading" });
        return true;
      } catch {
        setReturnError(
          active == null ? "nav.returnUnavailable" : "voice.return.unavailable",
        );
        historyTarget.current = null;
        return false;
      } finally {
        if (request === returnRequest.current) setReturning(false);
      }
    },
    [applyOverlay],
  );

  const returnToActiveTutor = useCallback(async () => {
    const moves = placeMovesRef.current;
    setReturnError(null);
    const pending = getActiveVoiceSnapshot();
    if (
      pending.starting &&
      pending.identity == null &&
      pending.pendingTarget != null
    ) {
      const target = pending.pendingTarget;
      const parked = parkedReader.current;
      if (
        parked != null &&
        parked.bookId === target.bookId &&
        (isBookSurface(parked) ? null : parked.jobId) === target.jobId
      )
        await restoreDestination({ kind: "reader", place: placeOf(parked) });
      else openTutor(target.bookId);
      return;
    }
    try {
      const identity = await synchronizeVoiceSession();
      if (identity == null || moves !== placeMovesRef.current) return;
      if (identity.bookId == null) {
        openTutor();
        return;
      }
      if (identity.jobId == null) {
        await restoreDestination(
          { kind: "reader", place: { kind: "pages", bookId: identity.bookId } },
          identity,
        );
      } else {
        const exports = await invoke<ExportSummary[]>("list_exports");
        if (
          moves !== placeMovesRef.current ||
          getActiveVoiceSnapshot().identity?.sessionId !== identity.sessionId
        )
          return;
        const currentExport =
          playbackItem?.jobId === identity.jobId ? playbackItem.id : null;
        const candidates = exports.filter(
          (e) =>
            e.jobId === identity.jobId &&
            e.bookId === identity.bookId &&
            e.format.toLowerCase() === "mp3" &&
            !e.stale,
        );
        const entry =
          candidates.find((e) => e.id === currentExport) ?? candidates[0];
        if (entry == null) {
          setReturnError("voice.return.unavailable");
          return;
        }
        await restoreDestination(
          {
            kind: "reader",
            place: {
              kind: "cast",
              bookId: identity.bookId,
              exportId: entry.id,
            },
          },
          identity,
        );
      }
    } catch {
      setReturnError("voice.return.failed");
    }
  }, [restoreDestination, playbackItem]);

  useEffect(() => {
    const returnHandler = () => {
      void returnToActiveTutor();
    };
    window.addEventListener(RETURN_TUTOR_EVENT, returnHandler);
    return () => window.removeEventListener(RETURN_TUTOR_EVENT, returnHandler);
  }, [returnToActiveTutor]);

  const travelHistory = (delta: -1 | 1) => {
    const index = history.index + delta;
    const destination = history.entries[index];
    if (destination == null) return;
    historyTarget.current = destinationKey(destination);
    setHistory({ ...history, index });
    const moves = placeMovesRef.current;
    void restoreDestination(destination).then((restored) => {
      if (!restored && moves === placeMovesRef.current) setHistory(history);
    });
  };
  const navigationControls = (
    <AppNavigationControls
      back={history.index > 0 && !returning}
      forward={history.index + 1 < history.entries.length && !returning}
      onBack={() => travelHistory(-1)}
      onForward={() => travelHistory(1)}
    />
  );

  const unseenNavigation = useRef<string | null>(null);
  useEffect(() => {
    const navigation = activeVoice.session.navigation;
    const identity = activeVoice.identity;
    if (
      identity == null ||
      navigation?.callId == null ||
      navigation.generation == null ||
      navigation.pageNo == null
    )
      return;
    const visible =
      overlay === "reading" &&
      readingItem?.bookId === identity.bookId &&
      (isBookSurface(readingItem) ? null : readingItem.jobId) ===
        identity.jobId;
    if (
      visible ||
      activeVoice.session.navigationOutcome?.callId === navigation.callId
    )
      return;
    const key = `${identity.sessionId}:${navigation.callId}:${navigation.generation}`;
    if (unseenNavigation.current === key) return;
    unseenNavigation.current = key;
    void invoke("voice_session_navigation_ack", {
      sessionId: identity.sessionId,
      callId: navigation.callId,
      generation: navigation.generation,
      pageNo: navigation.pageNo,
      disposition: "unavailable",
    }).catch(console.error);
  }, [activeVoice, overlay, readingItem]);

  useEffect(() => {
    const onBookOpen = (event: Event) => {
      const bookId = (event as CustomEvent<string>).detail;
      if (!bookId) return;
      const active = getActiveVoiceSnapshot();
      if (sessionIsActive(active) && active.identity?.bookId === bookId) {
        returnToTutor();
        return;
      }
      const movesAtStart = placeMovesRef.current;
      invoke<BookSummary[]>("list_books")
        .then((books) => {
          if (placeMovesRef.current !== movesAtStart) return;
          const resolved = resolveLastPlace(
            { kind: "pages", bookId },
            books,
            [],
          );
          if (resolved?.kind !== "pages") return;
          setBookSurface(resolved.surface);
          applyOverlay({ type: "openReading" });
        })
        .catch(console.error);
    };
    window.addEventListener(BOOK_OPEN_EVENT, onBookOpen);
    return () => window.removeEventListener(BOOK_OPEN_EVENT, onBookOpen);
  }, [applyOverlay]);

  useEffect(() => {
    invoke<string>("app_version").then(setVersion).catch(console.error);
    invoke<boolean>("get_onboarding_complete")
      .then(setOnboarded)
      .catch((error: unknown) => {
        console.error(error);
        setOnboarded(true);
      });
  }, []);

  useEffect(() => {
    if (onboarded !== true || bootPlace == null) return;
    if (/^#providers(\/|$)/.test(window.location.hash)) return;
    let active = true;
    const movesAtStart = placeMovesRef.current;
    Promise.all([
      invoke<ExportSummary[]>("list_exports"),
      invoke<BookSummary[]>("list_books"),
    ])
      .then(([exports, books]) => {
        if (!active || placeMovesRef.current !== movesAtStart) return;
        const shelf = buildAudioShelf(exports, books);
        const resolved = resolveLastPlace(bootPlace, books, shelf);
        if (resolved == null) {
          saveLastPlace(null);
        } else if (resolved.kind === "cast") {
          openSession(
            resolved.item,
            true,
            listenedOf(loadListened(), resolved.item.id),
          );
        } else {
          setBookSurface(resolved.surface);
          applyOverlay({ type: "openReading" });
        }
      })
      .catch(console.error);
    return () => {
      active = false;
    };
  }, [applyOverlay, bootPlace, onboarded, openSession]);

  useEffect(() => {
    if (bookSurface == null) return;
    let active = true;
    Promise.all([
      invoke<ExportSummary[]>("list_exports"),
      invoke<BookSummary[]>("list_books"),
    ])
      .then(([exports, books]) => {
        if (!active) return;
        const shelf = buildAudioShelf(exports, books);
        setAudioCheck({
          surface: bookSurface,
          noAudio: bookAvailability(bookSurface.bookId, shelf).playable == null,
        });
      })
      .catch(console.error);
    return () => {
      active = false;
    };
  }, [bookSurface]);

  const inBookRef = useRef(false);
  useEffect(() => {
    const open = overlay === "reading" ? (bookSurface ?? playbackItem) : null;
    if (open != null) {
      saveLastPlace(placeOf(open));
      inBookRef.current = true;
    } else if (inBookRef.current) {
      saveLastPlace(null);
      inBookRef.current = false;
    }
  }, [bookSurface, overlay, playbackItem]);

  const checklistRequestRef = useRef(0);

  const refreshChecklist = useCallback(() => {
    const requestId = ++checklistRequestRef.current;
    Promise.all([
      invoke<FullCatalog>("get_catalog"),
      invoke<InstalledModel[]>("list_installed_models"),
      invoke<BookSummary[]>("list_books"),
      invoke<JobSummary[]>("list_jobs"),
      invoke<string | null>("engine_active_variant").catch(() => null),
    ])
      .then(([catalog, installs, books, jobs, engineVariant]) => {
        if (requestId !== checklistRequestRef.current) return;
        setChecklist(
          buildSetupChecklist({
            models: catalog.models,
            installed: installedSet(installs),
            bookCount: books.length,
            jobCount: jobs.length,
            readerEngine: engineVariant === LITE_VARIANT,
          }),
        );
      })
      .catch(console.error);
  }, []);

  useEffect(() => {
    refreshChecklist();
  }, [refreshChecklist]);

  useEffect(() => {
    const onCreated = () => {
      setView("jobs");
      refreshChecklist();
    };
    const onGoView = (event: Event) => {
      const detail = (event as CustomEvent<View>).detail;
      if (detail) {
        setTrayOpen(false);
        // An explicit route request targets a MAIN view. App owns the
        // fixed full-screen overlays above it (reading), so
        // the request must dismiss whichever is open — otherwise the view
        // changes underneath while the overlay keeps covering it (AT-2-1:
        // BookDirect → Book tab → "Open in Library" left the reader up).
        // Same teardown as the reader's own close control.
        setResumeNotice(false);
        setBookSurface(null);
        applyOverlay({ type: "stop" });
        if (detail === "audio") setShelfFilter("audio");
        setView(detail);
      }
    };
    const onSetupChanged = () => refreshChecklist();
    window.addEventListener("txtcaster:job-created", onCreated);
    window.addEventListener(GO_VIEW_EVENT, onGoView);
    window.addEventListener(SETUP_CHANGED_EVENT, onSetupChanged);
    return () => {
      window.removeEventListener("txtcaster:job-created", onCreated);
      window.removeEventListener(GO_VIEW_EVENT, onGoView);
      window.removeEventListener(SETUP_CHANGED_EVENT, onSetupChanged);
    };
  }, [applyOverlay, refreshChecklist]);

  useEffect(() => {
    let active = true;
    const unlisten = listen("model-download-finished", () => {
      if (active) refreshChecklist();
    });
    return () => {
      active = false;
      unlisten.then((fn) => fn()).catch(console.error);
    };
  }, [refreshChecklist]);

  if (onboarded === null) return null;

  if (onboarded === false) {
    return (
      <div className="paper-grain flex h-screen items-start justify-center overflow-y-auto bg-ink-950 px-6 py-10 text-ink-100">
        <Wizard
          onDone={() => {
            setOnboarded(true);
            setView("library");
          }}
        />
      </div>
    );
  }

  return (
    <div className="paper-grain flex h-screen overflow-hidden bg-ink-950 text-ink-100">
      <a
        href="#main-content"
        data-testid="skip-to-content"
        onClick={(event) => {
          event.preventDefault();
          document.getElementById("main-content")?.focus();
        }}
        className="sr-only z-50 rounded-lg bg-ink-50 px-4 py-2 text-sm font-semibold text-ink-950 focus:not-sr-only focus:absolute focus:top-3 focus:start-3"
      >
        {t("nav.skipToContent")}
      </a>
      <LaunchIntro landing={bootPlace != null ? "fade" : "corner"} />
      <ShellRail
        view={view}
        version={version}
        onHome={() => {
          setShelfFilter("all");
          goToView("library");
        }}
        onSelect={(next) => {
          setTrayOpen(false);
          setView(next);
        }}
        onOpenListening={() => {
          setTrayOpen(false);
          goToAudioShelf();
        }}
        trayOpen={trayOpen}
        onToggleTray={() => setTrayOpen((open) => !open)}
        onOpenSearch={() => setPaletteOpen(true)}
        onOpenReportIssue={() => setReportIssueOpen(true)}
        engine={engine}
        studio={studio}
      />
      {trayOpen && (
        <ActivityTray
          studio={studio}
          engine={engine}
          onOpenProduction={() => {
            setTrayOpen(false);
            goToView("jobs");
          }}
          onClose={() => setTrayOpen(false)}
          elevated={overlay === "reading"}
          returnFocusTestId={
            overlay === "reading" ? "book-activity" : "nav-jobs"
          }
          anchor={overlay === "reading" ? "corner" : "rail"}
        />
      )}
      <div className="flex min-h-0 min-w-0 flex-1 flex-col">
        <AppUpdateBanner castsActive={castingBookIds.size > 0} />
        <div className="flex min-h-0 flex-1">
          <main
            id="main-content"
            tabIndex={-1}
            className="relative min-h-0 min-w-0 flex-1 overflow-hidden outline-none"
          >
            <div
              id="main-overlay"
              className="pointer-events-none absolute inset-0 z-30"
            />
            <div
              className={`h-full overflow-y-auto ${
                hasPlaybackItem && overlay === "none"
                  ? "pb-44 md:pb-32"
                  : "pb-24"
              }`}
            >
              {overlay === "none" && (
                <div className="flex min-w-0 flex-wrap items-center gap-3 px-5 py-2">
                  {navigationControls}
                  {sessionIsActive(activeVoice) &&
                    (activeVoice.identity != null ||
                      activeVoice.pendingTarget != null) && (
                      <button
                        type="button"
                        data-testid="return-to-tutor"
                        onClick={() => void returnToActiveTutor()}
                        disabled={returning}
                        className="flex min-h-10 min-w-0 items-center gap-2 rounded-lg bg-ink-850 px-3 text-secondary font-semibold text-ember-300 hover:bg-ink-800 disabled:opacity-50"
                      >
                        <span
                          aria-hidden="true"
                          className={
                            activeVoice.starting ? "ui-spin" : "ui-presence-dot"
                          }
                        />
                        <span>
                          {t(
                            activeVoice.identity?.purpose === "agent"
                              ? "voice.return.agent"
                              : activeVoice.starting
                                ? "voice.starting"
                                : "voice.return.action",
                          )}
                        </span>
                        {activeTitle !== "" && (
                          <bdi
                            dir="auto"
                            className="max-w-64 truncate font-normal text-ink-300"
                          >
                            {activeTitle}
                          </bdi>
                        )}
                      </button>
                    )}
                  {returnError != null && (
                    <div
                      role="status"
                      className="flex flex-wrap items-center gap-2 text-secondary text-wash-yellow-ink"
                    >
                      <span>{t(returnError)}</span>
                      {sessionIsActive(activeVoice) && (
                        <button
                          type="button"
                          data-testid="return-end-session"
                          onClick={() => {
                            void invoke("voice_session_stop").catch(
                              console.error,
                            );
                          }}
                        >
                          {t("voice.endSession")}
                        </button>
                      )}
                    </div>
                  )}
                </div>
              )}
              <CastDraftReturn />
              <div
                key={isSettingsSection(view) ? "settings" : view}
                className="animate-view-in min-h-full"
              >
                {(view === "library" || view === "audio") && (
                  <Library
                    filter={
                      filterSegment(shelfFilter) ===
                      (view === "audio" ? "audios" : "books")
                        ? shelfFilter
                        : view === "audio"
                          ? "audio"
                          : "all"
                    }
                    castingBookIds={castingBookIds}
                    onFilter={(next) => {
                      setShelfFilter(next);
                      setView(
                        filterSegment(next) === "audios" ? "audio" : "library",
                      );
                    }}
                    checklist={checklist}
                    onOpenAudio={(item) =>
                      openSession(
                        item,
                        false,
                        listenedOf(loadListened(), item.id),
                      )
                    }
                    onOpenSession={openSession}
                    onPickModels={() => setView("models")}
                    onOpenProviders={() => setView("providers")}
                  />
                )}
                {view === "jobs" && <JobsPanel studio={studio} />}
                {isSettingsSection(view) && (
                  <SettingsFrame section={view} onSelect={setView}>
                    {view === "models" && <ModelsPanel />}
                    {view === "providers" && <ProvidersPage />}
                    {view === "settings" && (
                      <Settings
                        onOpenReportIssue={() => setReportIssueOpen(true)}
                      />
                    )}
                  </SettingsFrame>
                )}
              </div>
            </div>
            {overlay === "none" && (
              <TutorLauncher
                open={
                  tutorPanel != null &&
                  !(tutorDock.mode === "floating" && tutorDock.folded)
                }
                raised={hasPlaybackItem}
                onClick={() => {
                  if (tutorPanel == null) {
                    if (sessionIsActive(getActiveVoiceSnapshot()))
                      void returnToActiveTutor();
                    else openTutor();
                  } else if (tutorDock.mode === "floating")
                    applyTutorDock({
                      ...tutorDock,
                      folded: !tutorDock.folded,
                    });
                  else closeTutorPanel();
                }}
              />
            )}
            {hasPlaybackItem && overlay === "none" && (
              <MiniPlayer
                onFollowAlong={
                  canFollowAlong(playbackItem)
                    ? () => applyOverlay({ type: "openReading" })
                    : null
                }
              />
            )}
          </main>
          {tutorPanel != null && overlay === "none" && (
            <TutorPanel
              key={tutorPanel}
              dock={tutorDock}
              onDock={applyTutorDock}
              onMoveCommit={storeDock}
              onClose={closeTutorPanel}
            />
          )}
        </div>
      </div>
      {paletteOpen && (
        <CommandPalette
          onClose={() => setPaletteOpen(false)}
          onChoose={choosePaletteTarget}
        />
      )}
      <PlaybackEngine />
      <EngineSetupSheet />
      <SleepTimerDriver />
      {overlay === "reading" && readingItem != null && (
        <ReadingView
          key={
            readingItem
              ? `${isBookSurface(readingItem) ? "direct" : (readingItem.jobId ?? "nojob")}:${readingItem.bookId}`
              : "reading-empty"
          }
          item={readingItem}
          checkpoint={checkpoints.current.get(checkpointKey(readingItem))}
          initialCompanionTab={
            activeVoice.identity?.bookId === readingItem.bookId &&
            activeVoice.identity?.purpose === "agent"
              ? "book"
              : undefined
          }
          onCheckpoint={(value) => {
            const key = checkpointKey(readingItem);
            checkpoints.current.delete(key);
            checkpoints.current.set(key, value);
            if (checkpoints.current.size > 20) {
              const active = getActiveVoiceSnapshot().identity;
              for (const [oldKey, checkpoint] of checkpoints.current) {
                if (checkpoint.sessionId !== active?.sessionId) {
                  checkpoints.current.delete(oldKey);
                  break;
                }
              }
              if (checkpoints.current.size > 20)
                checkpoints.current.delete(
                  checkpoints.current.keys().next().value!,
                );
            }
          }}
          navigationControls={navigationControls}
          onClose={() => {
            setResumeNotice(false);
            setBookSurface(null);
            setTrayOpen(false);
            applyOverlay({ type: "closeReading" });
            setView("library");
          }}
          notice={
            bookSurface != null &&
            audioCheck?.surface === bookSurface &&
            audioCheck.noAudio ? (
              <div
                data-testid="book-cast-notice"
                className="flex shrink-0 flex-wrap items-center justify-between gap-x-4 gap-y-2 border-b border-ink-800/60 px-5 py-2"
              >
                <p className="min-w-0 text-secondary text-ink-300">
                  {t("reader.noAudioYet")}
                </p>
                <button
                  type="button"
                  data-testid="book-cast-action"
                  onClick={() => goToCastBook(bookSurface.bookId)}
                  className="shrink-0 whitespace-nowrap rounded-lg bg-ink-50 px-3 py-1.5 text-secondary font-semibold text-ink-950 transition-transform duration-200 active:scale-[0.98]"
                >
                  {t("reader.castThisBook")}
                </button>
              </div>
            ) : undefined
          }
          utilities={
            <>
              <ShellActivityPill
                testId="book-activity"
                active={trayOpen}
                engine={engine}
                studio={studio}
                onToggle={() => setTrayOpen((open) => !open)}
              />
              <button
                type="button"
                data-testid="book-settings"
                aria-label={t("nav.settings")}
                title={t("nav.settingsTitle")}
                onClick={() => goToView("settings")}
                className="flex h-9 w-9 shrink-0 items-center justify-center rounded-full text-ink-400 transition-colors hover:bg-ink-800 hover:text-ink-200 active:scale-[0.96]"
              >
                <SettingsIcon className="h-4 w-4" />
              </button>
            </>
          }
          onRepair={() => {
            setResumeNotice(false);
            setBookSurface(null);
            applyOverlay({ type: "closeReading" });
            goToView("jobs");
          }}
          resumeNotice={resumeNotice}
          onDismissResumeNotice={() => setResumeNotice(false)}
        />
      )}
      <ReportIssueModal
        open={reportIssueOpen}
        onClose={() => setReportIssueOpen(false)}
        version={version}
      />
    </div>
  );
}

export default App;
