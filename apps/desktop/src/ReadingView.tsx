import {
  useCallback,
  useEffect,
  useMemo,
  useReducer,
  useRef,
  useState,
  useSyncExternalStore,
} from "react";
import { createPortal } from "react-dom";
import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";

import AudioPlayer from "./AudioPlayer";
import ReadingFacsimile from "./ReadingFacsimile";
import { FOLLOW_ALONG_FALLBACK } from "./readingFocusRestore";
import { isTopDialog, useDialogFocus } from "./useDialogFocus";
import ReadingHeader from "./ReadingHeader";
import ReadingOutline, {
  loadOutlineOpen,
  storeOutlineOpen,
} from "./ReadingOutline";
import StudyCompanion, { BookPanel, type CompanionTab } from "./StudyCompanion";
import {
  RAIL_DEFAULT_PX,
  RAIL_MIN_PX,
  clampRailWidth,
  loadRailWidth,
  railMaxPx,
  resetRailWidth,
  storeRailWidth,
} from "./tutorRailModel";
import {
  FLOAT_MAX_WIDTH_PX,
  type TutorDock,
  clampFloat,
  defaultFloat,
  floatHeight,
  loadDock,
  storeDock,
} from "./tutorDockModel";
import { ChevronIcon, SparkIcon } from "./Icons";
import PlayerCapsule from "./PlayerCapsule";
import ReadingPageControls, { ReadingPageEdges } from "./ReadingPageControls";
import ReadingPageOverlay from "./ReadingPageOverlay";
import ReadingPageZoom from "./ReadingPageZoom";
import ReadingScript from "./ReadingScript";
import ReadingSpotlight from "./ReadingSpotlight";
import AgentCitationOverlay from "./AgentCitationOverlay";
import BookAgentPanel from "./BookAgentPanel";
import {
  ackCitation,
  resolveAck,
  agentCitationPlan,
  AGENT_HIGHLIGHT_CLEAR_EVENT,
  AGENT_HIGHLIGHT_EVENT,
} from "./agentCitationApplier";
import {
  type AgentCitation,
  agentCitationFrom,
  agentCitationReducer,
  INITIAL_AGENT_CITATION,
  type AgentCitationAction,
  type AgentCitationState,
} from "./agentHighlightModel";
import ReadingSuppressed from "./ReadingSuppressed";
import type { SuppressedBlock } from "./readingSuppressedModel";
import ReadingTapLayer from "./ReadingTapLayer";
import ReadingThreshold from "./ReadingThreshold";
import ReadingUnderline from "./ReadingUnderline";
import VoiceSessionPanel from "./VoiceSessionPanel";
import type { AudioShelfItem } from "./audioShelfModel";
import type { BookSummary } from "./libraryModel";
import { browserScheduler } from "./browserScheduler";
import { audioTimeMs } from "./playbackElement";
import { createChapterCache } from "./readingChapterCache";
import { createReadingClock } from "./readingClock";
import { createHighlightApplier } from "./readingHighlight";
import { boxSignature, createPageOverlayApplier } from "./readingPageHighlight";
import {
  aspectRatio,
  type FigurePageHold,
  figureForSegment,
  pageToShow,
  type TravelDirection,
  updateFigureHold,
} from "./readingFacsimileModel";
import {
  fittedWidthPx,
  loadPageFit,
  loadPageZoom,
  storePageFit,
  storePageZoom,
  SURFACE_GUTTER_PX,
  zoomIn as zoomInFrom,
  zoomOut as zoomOutFrom,
  clampPageZoom,
  type PageFit,
} from "./readingPageFit";
import {
  createAutoScrollApplier,
  figureScrollTop,
  isScrollKey,
  topicScrollTop,
} from "./readingPageScroll";
import {
  FOLLOWING,
  followReducer,
  seekNonceAdvanced,
  shownPage,
  travelDirectionAfter,
  type FollowAction,
  type FollowContext,
  type FollowState,
} from "./followModel";
import {
  loadReadingMode,
  NO_OVERLAY,
  overlayPlan,
  pageModeAdvisory,
  pageModeReason,
  storeReadingMode,
  type OverlayPlan,
  type ReadingMode,
  type TapOutcome,
} from "./readingPageModel";
import {
  pageModeSample,
  reportPageMode,
  shouldEmitPageMode,
  type PageModeSample,
} from "./readingPageDiagnostics";
import {
  anchorPage,
  anchorTarget,
  INITIAL_TEACHER_ANCHOR,
  isRegionKind,
  loadSpotlightEnabled,
  spotlightPlan,
  teacherAnchorFrom,
  teacherAnchorReducer,
  type TeacherAnchorAction,
  type TeacherAnchorState,
} from "./teacherAnchorModel";
import {
  followTransitionLine,
  reportFollowState,
  reportReadingTap,
  tapLine,
} from "./readingFollowDiagnostics";
import {
  decideEmission,
  INITIAL_SUPPRESSION,
  readingClockScope,
  reportPlaybackEvent,
  reportReadingClock,
  sampleReadingClock,
} from "./readingClockDiagnostics";
import {
  chapterToLoadAt,
  segmentAt,
  type ReadingChapter,
  type ReadingCursor,
  type ReadingFigure,
  type ReadingIndex,
  type ReadingPage,
} from "./readingModel";
import {
  subscribe as subscribePlayback,
  dispatch as dispatchPlayback,
  getSnapshot,
} from "./playbackStore";
import {
  getActiveVoiceSnapshot,
  matchesVoiceScope,
  subscribeVoice,
} from "./voiceSessionStore";
import { INITIAL_VOICE_SESSION } from "./voiceSessionModel";
import type {
  ReaderCheckpoint,
  NavigationCheckpoint,
} from "./readerCheckpoint";
import {
  displayedPageReport,
  isBookSurface,
  type BookSurfaceItem,
} from "./viewportSyncModel";
import { usePlayback } from "./usePlayback";
import { useTutorNavigation } from "./useTutorNavigation";
import TutorNavigationControls from "./TutorNavigationControls";
import type { VoiceNavigation } from "./voiceSessionModel";
import { FetchBackoff } from "./readingFetchBackoff";
import { F11_STRINGS } from "./f11Strings";
import type { OpenTarget } from "./sessionHistoryStore";
import { t as tr, useI18n } from "./i18n";
import { READER_SHOW_TUTOR_EVENT } from "./views";

const REPORT_INTERVAL_MS = 2000;

interface ReadingViewProps {
  checkpoint?: ReaderCheckpoint;
  initialCompanionTab?: CompanionTab;
  onCheckpoint?: (value: ReaderCheckpoint) => void;
  navigationControls?: React.ReactNode;
  /**
   * The playing export (the follow-along overlay), or the job-independent
   * BookSurface mount (F8 §2.2 D2): cached page images beside the voice
   * panel, narration/word-overlay machinery disabled — a BookDirect voice
   * session's viewport.
   */
  item: AudioShelfItem | BookSurfaceItem;
  onClose: () => void;
  onRepair: () => void;
  resumeNotice?: boolean;
  onDismissResumeNotice?: () => void;
  utilities?: React.ReactNode;
  notice?: React.ReactNode;
}

function ReadingView({
  item,
  onClose,
  onRepair,
  resumeNotice = false,
  onDismissResumeNotice,
  utilities,
  notice,
  checkpoint,
  onCheckpoint,
  navigationControls,
  initialCompanionTab,
}: ReadingViewProps) {
  const { t } = useI18n();
  // The BookSurface (jobless) mount: no reading index, no clock, no audio —
  // pages render from the book cache and the session's Navigation events
  // (not narration cursors) drive the follow model.
  const direct = isBookSurface(item);
  const itemJobId = direct ? null : item.jobId;
  const jobId = itemJobId ?? "";
  const [index, setIndex] = useState<ReadingIndex | null>(null);
  const [suppressed, setSuppressed] = useState<SuppressedBlock[]>([]);
  const [chapter, setChapter] = useState<ReadingChapter | null>(null);
  const [segmentIdx, setSegmentIdx] = useState<number | null>(null);
  const [loadError, setLoadError] = useState<string | null>(null);
  const engineError = usePlayback((state) => state.errorMessage);
  const [pages, setPages] = useState<Map<number, ReadingPage>>(new Map());
  const [figures, setFigures] = useState<ReadingFigure[]>([]);
  const inFlightPages = useRef<Set<number>>(new Set());
  const pagesBookRef = useRef<string>(item.bookId);

  const [storedMode, setMode] = useState<ReadingMode>(
    () => checkpoint?.mode ?? loadReadingMode(),
  );
  // BookSurface has no script (no narration segments): the page surface is
  // the only honest mode, whatever preference is stored.
  const mode: ReadingMode = direct ? "page" : storedMode;
  const [outlineOpen, setOutlineOpen] = useState<boolean>(loadOutlineOpen);
  const [focusMode, setFocusMode] = useState(checkpoint?.focusMode ?? false);
  const [companionTab, setCompanionTab] = useState<CompanionTab>(
    initialCompanionTab ?? checkpoint?.companionTab ?? "tutor",
  );
  const [historyBrowse, setHistoryBrowse] = useState(false);
  const [railWidth, setRailWidth] = useState<number>(() =>
    loadRailWidth(window.innerWidth),
  );
  // Commit fires synchronously after a keyboard resize, before the state
  // re-render — the ref carries the just-applied value.
  const railWidthRef = useRef(railWidth);
  const [viewportWidth, setViewportWidth] = useState(() => window.innerWidth);
  const [viewportHeight, setViewportHeight] = useState(
    () => window.innerHeight,
  );
  const [tutorDock, setTutorDock] = useState<TutorDock>(loadDock);
  const tutorDockRef = useRef(tutorDock);
  const applyTutorDock = (next: TutorDock) => {
    tutorDockRef.current = next;
    setTutorDock(next);
    storeDock(next);
  };
  const floatSize = {
    width: Math.min(railWidth, FLOAT_MAX_WIDTH_PX, viewportWidth - 32),
    height: floatHeight(viewportHeight),
  };
  const [book, setBook] = useState<BookSummary | null>(null);
  const [wideShell, setWideShell] = useState(
    () => window.matchMedia("(min-width: 1024px)").matches,
  );
  const activeVoice = useSyncExternalStore(
    subscribeVoice,
    getActiveVoiceSnapshot,
  );
  const voiceSession = matchesVoiceScope(item.bookId, itemJobId)
    ? activeVoice.session
    : INITIAL_VOICE_SESSION;
  const tutorSessionLive =
    voiceSession.status === "live" || voiceSession.status === "renewing";
  const tutorLiveRef = useRef(tutorSessionLive);
  const tutorGuiding = tutorSessionLive && voiceSession.purpose !== "agent";
  const tutorGuidingRef = useRef(tutorGuiding);
  tutorGuidingRef.current = tutorGuiding;
  const sessionWasLive = useRef(false);
  const outlineOpenRef = useRef(outlineOpen);
  const focusModeRef = useRef(focusMode);

  useEffect(() => {
    const wide = window.matchMedia("(min-width: 1024px)");
    const onChange = (event: MediaQueryListEvent) =>
      setWideShell(event.matches);
    wide.addEventListener("change", onChange);
    return () => wide.removeEventListener("change", onChange);
  }, []);

  useEffect(() => {
    outlineOpenRef.current = outlineOpen;
    focusModeRef.current = focusMode;
  }, [outlineOpen, focusMode]);

  const [fit, setFit] = useState<PageFit>(
    () => checkpoint?.fit ?? loadPageFit(),
  );
  const [zoom, setZoom] = useState<number>(
    () => checkpoint?.zoom ?? loadPageZoom(),
  );
  const [, renderNarrationPage] = useReducer((count: number) => count + 1, 0);
  const [surfaceBox, setSurfaceBox] = useState<{
    width: number;
    height: number;
  }>({ width: 0, height: 0 });
  const scrollRef = useRef<HTMLDivElement>(null);
  const [followState, setFollowState] = useState<FollowState>(
    checkpoint?.page != null
      ? { mode: "browsing", pageNo: checkpoint.page }
      : FOLLOWING,
  );
  const followRef = useRef<FollowState>(followState);
  const followContextRef = useRef<FollowContext>({
    narrationPage: null,
    lastPage: 0,
  });
  const travelRef = useRef<TravelDirection>(1);
  const itemMode =
    !direct && "mode" in item && typeof item.mode === "string" ? item.mode : "";
  const chapterMode =
    chapter != null && typeof chapter.mode === "string" ? chapter.mode : "";
  const effectiveMode = (chapterMode || itemMode).toLowerCase();
  const isTeacherJob = effectiveMode === "teacher" || effectiveMode === "teach";
  const reason = pageModeReason(direct ? "" : item.mode, item.isScanned);
  const panelRef = useRef<HTMLDivElement>(null);
  const scriptRef = useRef<HTMLDivElement>(null);
  const underlineRef = useRef<HTMLDivElement>(null);
  const overlayWordRef = useRef<HTMLDivElement>(null);
  const overlayFallbackRef = useRef<HTMLDivElement>(null);
  const overlayInputs = useRef<{
    mode: ReadingMode;
    pages: Map<number, ReadingPage>;
    figures: ReadingFigure[];
    isTeacher: boolean;
  }>({ mode, pages: new Map(), figures: [], isTeacher: isTeacherJob });
  const retriedChapters = useRef<Set<string>>(new Set());
  const lastPageMode = useRef<PageModeSample | null>(null);
  const chapterRef = useRef<ReadingChapter | null>(null);
  const wantedChapterRef = useRef<string | null>(null);
  const inFlightChapters = useRef<Set<string>>(new Set());
  const segmentRef = useRef<number | null>(null);
  const chapterIdxRef = useRef<number | null>(null);
  const figureHoldRef = useRef<FigurePageHold | null>(null);
  const lastSeekNonceRef = useRef<number>(getSnapshot().seekNonce);
  const renderCountRef = useRef(0);
  renderCountRef.current += 1;
  const cache = useMemo(() => createChapterCache(), []);
  const chapterBackoff = useMemo(() => new FetchBackoff(), []);
  const applier = useMemo(() => createHighlightApplier(), []);
  const overlayApplier = useMemo(() => createPageOverlayApplier(), []);
  const autoScroller = useMemo(() => createAutoScrollApplier(), []);

  const wasTutorLiveRef = useRef(false);

  useEffect(() => {
    tutorLiveRef.current = tutorSessionLive;
    if (direct) return; // no audio plane to pause or resync
    if (tutorSessionLive) {
      wasTutorLiveRef.current = true;
      dispatchPlayback({ type: "pause" });
      applier.apply(
        { container: scriptRef.current, underline: underlineRef.current },
        null,
        0,
      );
      overlayApplier.apply(
        { word: overlayWordRef.current, fallback: overlayFallbackRef.current },
        NO_OVERLAY,
      );
      return;
    }
    if (!wasTutorLiveRef.current) return;
    wasTutorLiveRef.current = false;
    const atMs = audioTimeMs();
    if (Number.isFinite(atMs)) {
      dispatchPlayback({ type: "seekMs", ms: atMs });
    }
  }, [applier, direct, overlayApplier, tutorSessionLive]);

  const dispatchFollow = useCallback((action: FollowAction) => {
    const from = followRef.current;
    const to = followReducer(from, action, followContextRef.current);
    const line = followTransitionLine({ trigger: action.type, from, to });
    travelRef.current = travelDirectionAfter(
      action,
      line != null,
      travelRef.current,
    );
    if (line == null) return;
    followRef.current = to;
    setFollowState(to);
    void reportFollowState(line);
  }, []);

  // Teacher-mode anchoring (VOICE_AGENT §11.1): the live tutor's anchors,
  // surfaced by the event pump (only live generations reach this event),
  // drive the shown page and the spotlight through their own reducer.
  const [anchorState, setAnchorState] = useState<TeacherAnchorState>(
    INITIAL_TEACHER_ANCHOR,
  );
  const anchorRef = useRef<TeacherAnchorState>(INITIAL_TEACHER_ANCHOR);
  const spotlightEnabled = useMemo(loadSpotlightEnabled, []);
  const spotlightOutlineRef = useRef<HTMLDivElement>(null);
  const scrolledSyncNonce = useRef(0);
  const absentAnchorRegions = useRef<Set<string>>(new Set());

  const dispatchAnchor = useCallback((action: TeacherAnchorAction) => {
    const next = teacherAnchorReducer(anchorRef.current, action);
    if (next === anchorRef.current) return;
    anchorRef.current = next;
    setAnchorState(next);
  }, []);

  useEffect(() => {
    let active = true;
    const unlisten = listen("voice-teacher-anchor", (event) => {
      if (!active) return;
      const anchor = teacherAnchorFrom(event.payload);
      if (anchor != null) dispatchAnchor({ type: "anchor", anchor });
    });
    return () => {
      active = false;
      unlisten.then((fn) => fn()).catch(console.error);
    };
  }, [dispatchAnchor]);

  // Book Agent citation channel (charter §4.5, B-4): playhead-decoupled,
  // generation-ordered, one active citation. The ack answers the Rust
  // highlight port with what the surface ACTUALLY did (the B-1 gate's law).
  const [citationState, setCitationState] = useState<AgentCitationState>(
    INITIAL_AGENT_CITATION,
  );
  const citationRef = useRef<AgentCitationState>(INITIAL_AGENT_CITATION);
  const modeRef = useRef<string>("script");
  const shownPageRef = useRef<number | null>(null);
  const segmentStartRef = useRef<number>(0);
  const dispatchCitation = useCallback((action: AgentCitationAction) => {
    const next = agentCitationReducer(citationRef.current, action);
    if (next === citationRef.current) return;
    citationRef.current = next;
    setCitationState(next);
  }, []);
  const pendingAckRef = useRef<AgentCitation | null>(null);
  /** Terminal settlement: any path that tears citation state down answers
   * the still-pending ack honestly first — a stale ref must never bleed
   * into the next session's first citation. */
  const settlePendingAck = useCallback((reason: string) => {
    const pending = pendingAckRef.current;
    if (pending == null) return;
    pendingAckRef.current = null;
    ackCitation(pending.ackCallId, false, reason);
  }, []);
  // Any session ending (End button, provider failure, expiry) clears the
  // Agent citation and its generation watermark: generations are
  // per-session, so a stale watermark would silently swallow the next
  // session's citations.
  useEffect(() => {
    if (tutorSessionLive) {
      sessionWasLive.current = true;
    } else if (sessionWasLive.current) {
      sessionWasLive.current = false;
      settlePendingAck("session_ended");
      dispatchCitation({ type: "sessionEnd" });
    }
  }, [tutorSessionLive, dispatchCitation, settlePendingAck]);
  useEffect(() => {
    let active = true;
    const unlisten = listen(AGENT_HIGHLIGHT_EVENT, (event) => {
      if (!active) return;
      const citation = agentCitationFrom(event.payload);
      if (citation == null) return;
      // Reducer-rejected staleness answers immediately (the render can never
      // show it); everything else resolves AFTER the render commit, from the
      // exact plan the overlay draws (Hermes finale item 2).
      if (citation.gen < citationRef.current.highWaterGen) {
        ackCitation(citation.ackCallId, false, "stale_generation");
        return;
      }
      // A newer citation supersedes an unresolved pending ack: settle the
      // old one honestly before adopting the new (one pending, no lost acks).
      const previous = pendingAckRef.current;
      if (previous != null && previous.ackCallId !== citation.ackCallId) {
        ackCitation(previous.ackCallId, false, "superseded_by_newer_citation");
      }
      pendingAckRef.current = citation;
      dispatchCitation({ type: "citation", citation });
    });
    const unlistenClear = listen(AGENT_HIGHLIGHT_CLEAR_EVENT, () => {
      if (!active) return;
      settlePendingAck("session_ended");
      dispatchCitation({ type: "sessionEnd" });
    });
    return () => {
      active = false;
      settlePendingAck("view_teardown");
      unlisten.then((fn) => fn()).catch(console.error);
      unlistenClear.then((fn) => fn()).catch(console.error);
    };
  }, [dispatchCitation, settlePendingAck]);

  const [sessionPage, setSessionPage] = useState<number | null>(
    checkpoint?.page ?? null,
  );
  const tutorReturnRef = useRef<(() => void) | null>(null);
  const applyTutorNavigation = useCallback(
    (navigation: VoiceNavigation) => {
      const page = navigation.pageNo;
      if (page == null) return;
      followContextRef.current.narrationPage = page;
      dispatchAnchor({ type: "release" });
      dispatchFollow({ type: "returnToNarration" });
      dispatchFollow({ type: "narrationPage", pageNo: page });
      setSessionPage(page);
    },
    [dispatchAnchor, dispatchFollow],
  );

  // Session-start page-mode steer (§11.1 scope, the runtime's one nudge):
  // the spotlight contract lives on the page surface. A steer, not a lock —
  // the user can toggle straight back.
  useEffect(() => {
    if (mode === "page") return;
    let active = true;
    const unlisten = listen("voice-page-mode-steer", () => {
      if (!active) return;
      dispatchFollow({ type: "readingModeChanged" });
      setMode(() => {
        storeReadingMode("page");
        return "page";
      });
    });
    return () => {
      active = false;
      unlisten.then((fn) => fn()).catch(console.error);
    };
  }, [mode, dispatchFollow]);

  // Region anchors resolve against layout regions the view may not hold yet
  // (the chapter effect loads only the figures its segments cite): fetch the
  // anchored region by id, once per in-flight id, and remember ids the book
  // answered empty — that non-answer is the §5 existence check failing, and
  // the page stays put.
  const latestAnchor = anchorState.latest;
  useEffect(() => {
    if (latestAnchor == null || !isRegionKind(latestAnchor.kind)) return;
    const id = latestAnchor.id;
    if (figures.some((entry) => entry.figureId === id)) return;
    if (absentAnchorRegions.current.has(id)) return;
    absentAnchorRegions.current.add(id);
    let cancelled = false;
    invoke<ReadingFigure[]>("get_reading_figures", {
      bookId: item.bookId,
      figureIds: [id],
    })
      .then((payload) => {
        if (cancelled) return;
        if (payload.length === 0) return; // truly absent: never refetch
        absentAnchorRegions.current.delete(id);
        setFigures((known) => [
          ...known.filter(
            (entry) =>
              !payload.some((added) => added.figureId === entry.figureId),
          ),
          ...payload,
        ]);
      })
      .catch(() => {
        absentAnchorRegions.current.delete(id);
      });
    return () => {
      cancelled = true;
    };
  }, [figures, item.bookId, latestAnchor]);

  useEffect(() => {
    if (jobId === "") return; // BookSurface: no job, no reading index
    let cancelled = false;
    invoke<ReadingIndex>("get_reading_index", { jobId })
      .then((payload) => {
        if (!cancelled) setIndex(payload);
      })
      .catch((cause: unknown) => {
        if (!cancelled) {
          setLoadError(
            typeof cause === "string" ? cause : tr("reader.cannotRead"),
          );
        }
      });
    return () => {
      cancelled = true;
    };
  }, [jobId]);

  useEffect(() => {
    if (jobId === "") return;
    let cancelled = false;
    invoke<SuppressedBlock[]>("list_suppressed_blocks", { jobId })
      .then((records) => {
        if (!cancelled) setSuppressed(records);
      })
      .catch(() => {
        if (!cancelled) setSuppressed([]);
      });
    return () => {
      cancelled = true;
    };
  }, [jobId]);

  useEffect(() => {
    if (index == null) return;
    if (engineError != null) return;
    const fetchChapter = (chapterId: string, display: boolean) => {
      const cached = cache.get(jobId, chapterId);
      if (cached) {
        if (display) {
          chapterRef.current = cached;
          setChapter(cached);
        }
        const retryable =
          display &&
          cached.anchorGeometry === "unavailable" &&
          !retriedChapters.current.has(chapterId);
        if (!retryable) return;
      }
      if (inFlightChapters.current.has(chapterId)) return;
      if (!chapterBackoff.canTry(chapterId, performance.now())) return;
      if (cached) retriedChapters.current.add(chapterId);
      inFlightChapters.current.add(chapterId);
      invoke<ReadingChapter>("get_reading_chapter", { jobId, chapterId })
        .then((payload) => {
          chapterBackoff.succeeded(chapterId);
          cache.set(jobId, chapterId, payload);
          if (wantedChapterRef.current === chapterId) {
            chapterRef.current = payload;
            setChapter(payload);
          }
        })
        .catch(() => {
          chapterBackoff.failed(chapterId, performance.now());
        })
        .finally(() => {
          inFlightChapters.current.delete(chapterId);
        });
    };

    const onCursor = (
      cursor: ReadingCursor,
      stale: boolean,
      timeMs: number,
    ) => {
      const position = Number.isFinite(timeMs)
        ? chapterToLoadAt(index, timeMs)
        : null;
      const wantedChapterId =
        position != null ? index.chapters[position]?.chapterId : null;
      const chapterMatch =
        wantedChapterId == null ||
        chapterRef.current?.chapterId === wantedChapterId;
      const inputs = overlayInputs.current;
      const scriptWordIdx =
        stale || tutorLiveRef.current ? null : cursor.heldWordIdx;
      applier.apply(
        { container: scriptRef.current, underline: underlineRef.current },
        scriptWordIdx,
        cursor.fill,
      );
      const active =
        cursor.segmentIdx == null || !chapterMatch
          ? null
          : (chapterRef.current?.segments[cursor.segmentIdx] ?? null);
      if (!chapterMatch) {
        figureHoldRef.current = null;
      }
      const currentSeekNonce = getSnapshot().seekNonce;
      const isSeek = currentSeekNonce !== lastSeekNonceRef.current;
      if (isSeek) {
        lastSeekNonceRef.current = currentSeekNonce;
      }
      const isSegmentTransition =
        cursor.segmentIdx != null &&
        segmentRef.current != null &&
        cursor.segmentIdx !== segmentRef.current;
      const isContiguous =
        !isSeek &&
        segmentRef.current != null &&
        cursor.segmentIdx != null &&
        (cursor.segmentIdx === segmentRef.current ||
          cursor.segmentIdx === segmentRef.current + 1);

      let narration = pageToShow(active, inputs.figures);
      if (inputs.isTeacher) {
        const { hold, pageNo } = updateFigureHold(
          figureHoldRef.current,
          active,
          inputs.figures,
          isContiguous,
          isSegmentTransition,
        );
        figureHoldRef.current = hold;
        if (pageNo != null) {
          narration = pageNo;
        }
      } else {
        figureHoldRef.current = null;
      }

      if (narration !== followContextRef.current.narrationPage) {
        followContextRef.current.narrationPage = narration;
        dispatchFollow({ type: "narrationPage", pageNo: narration });
        renderNarrationPage();
      }
      const shown = shownPage(followRef.current, narration);
      const surface = shown == null ? null : (inputs.pages.get(shown) ?? null);
      const plan =
        inputs.mode === "page" &&
        !tutorLiveRef.current &&
        cursor.inSegment &&
        !inputs.isTeacher
          ? overlayPlan(
              active,
              stale ? null : cursor.heldWordIdx,
              shown,
              surface,
              inputs.isTeacher,
            )
          : NO_OVERLAY;
      overlayApplier.apply(
        { word: overlayWordRef.current, fallback: overlayFallbackRef.current },
        plan,
      );
      if (inputs.mode === "page") {
        if (inputs.isTeacher) {
          const container = scrollRef.current;
          if (
            container != null &&
            active != null &&
            shown != null &&
            surface != null
          ) {
            const figure = figureForSegment(active, inputs.figures);
            if (
              figure != null &&
              figure.pageNo === shown &&
              figure.bbox != null &&
              (surface.heightPt ?? 0) > 0
            ) {
              const [, y0, , y1] = figure.bbox;
              const figureCenterFraction = (y0 + y1) / (2 * surface.heightPt!);
              const figureCenterPx =
                container.scrollHeight * figureCenterFraction;
              const targetTop = figureScrollTop(
                figureCenterPx,
                container.clientHeight,
                container.scrollHeight,
              );
              autoScroller.syncTeacher(
                container,
                targetTop,
                followRef.current.mode === "following",
              );
            } else if (active.rects.length > 0 && (surface.heightPt ?? 0) > 0) {
              const blockY0Fraction = active.rects[0].y0 / surface.heightPt!;
              const blockTopPx = container.scrollHeight * blockY0Fraction;
              const targetTop = topicScrollTop(
                blockTopPx,
                container.clientHeight,
                container.scrollHeight,
                container.scrollTop,
              );
              autoScroller.syncTeacher(
                container,
                targetTop,
                followRef.current.mode === "following",
              );
            }
          }
        } else {
          autoScroller.apply(
            {
              container: scrollRef.current,
              word: overlayWordRef.current,
              envelope: overlayFallbackRef.current,
            },
            {
              following: followRef.current.mode === "following",
              pageNo: shown,
              wordSignature: boxSignature(plan),
            },
          );
        }
      }
      if (cursor.segmentIdx !== segmentRef.current) {
        segmentRef.current = cursor.segmentIdx;
        setSegmentIdx(cursor.segmentIdx);
      }
      if (position == null) return;
      const wanted = index.chapters[position].chapterId;
      chapterIdxRef.current = index.chapters[position].idx;
      if (chapterRef.current?.chapterId !== wanted) {
        wantedChapterRef.current = wanted;
        fetchChapter(wanted, true);
      }
      const next = index.chapters[position + 1];
      if (next != null) fetchChapter(next.chapterId, false);
    };

    const clock = createReadingClock({
      scheduler: browserScheduler(),
      getTimeMs: audioTimeMs,
      getChapter: () => chapterRef.current,
      onCursor,
      subscribeResyncTriggers: (listener) => {
        let seekNonce = getSnapshot().seekNonce;
        let rate = getSnapshot().rate;
        return subscribePlayback(() => {
          const next = getSnapshot();
          if (next.seekNonce === seekNonce && next.rate === rate) return;
          seekNonce = next.seekNonce;
          rate = next.rate;
          listener();
        });
      },
      flushMemo: () => {
        cache.flush();
        applier.reset();
      },
    });

    clock.start();
    const scope = readingClockScope(
      index.chapters.length,
      index.chapters.reduce((total, entry) => total + entry.segmentCount, 0),
      `origin=${index.origin} totalMs=${index.totalMs}`,
    );
    let previous = clock.stats();
    let since = Date.now();
    let suppression = INITIAL_SUPPRESSION;
    const reporter = window.setInterval(() => {
      const stats = clock.takeWindow();
      const elapsed = Date.now() - since;
      const sample = sampleReadingClock({
        chapterIdx: chapterIdxRef.current,
        mediaMs: stats.lastTimeMs,
        ticks: stats.ticks - previous.ticks,
        resyncCount: stats.resyncs,
        deltaSumMs: stats.deltaSumMs - previous.deltaSumMs,
        deltaMaxMs: stats.deltaMaxMs,
        zeroDeltaTicks: stats.zeroDeltaTicks - previous.zeroDeltaTicks,
        elapsedMs: elapsed,
        offsetMs: Number.isFinite(stats.lastWordOffsetMs)
          ? stats.lastWordOffsetMs
          : null,
      });
      const decision = decideEmission(suppression, sample);
      suppression = decision.next;
      if (decision.emit) {
        void reportReadingClock(
          sample,
          scope,
          decision.heartbeat ? decision.suppressedRun : 0,
        );
      }
      previous = stats;
      since = Date.now();
    }, REPORT_INTERVAL_MS);
    return () => {
      window.clearInterval(reporter);
      clock.stop();
      const last = clock.stats().lastTimeMs;
      if (Number.isFinite(last)) {
        void reportPlaybackEvent("exit", last / 1000);
      }
    };
  }, [
    applier,
    autoScroller,
    cache,
    chapterBackoff,
    dispatchFollow,
    engineError,
    index,
    jobId,
    overlayApplier,
  ]);

  useEffect(() => {
    const previous = pagesBookRef.current;
    pagesBookRef.current = item.bookId;
    if (previous === item.bookId) return;
    inFlightPages.current.clear();
    setPages(new Map());
  }, [item.bookId]);

  const onNeedPages = useCallback(
    (wanted: number[]) => {
      const missing = wanted.filter(
        (number) => !inFlightPages.current.has(number),
      );
      if (missing.length === 0) return;
      for (const number of missing) inFlightPages.current.add(number);
      const issuedFor = item.bookId;
      invoke<ReadingPage[]>("ensure_reading_pages", {
        bookId: issuedFor,
        pages: missing,
      })
        .then((payload) => {
          if (pagesBookRef.current !== issuedFor) return;
          setPages((current) => {
            const next = new Map(current);
            for (const entry of payload) next.set(entry.pageNo, entry);
            return next;
          });
          for (const entry of payload) {
            if (entry.imagePath == null)
              inFlightPages.current.delete(entry.pageNo);
          }
          const answered = new Set(payload.map((entry) => entry.pageNo));
          for (const number of missing) {
            if (!answered.has(number)) inFlightPages.current.delete(number);
          }
        })
        .catch(() => {
          if (pagesBookRef.current !== issuedFor) return;
          for (const number of missing) inFlightPages.current.delete(number);
        });
    },
    [item.bookId],
  );

  useEffect(() => {
    if (chapter == null) return;
    const wanted = [
      ...new Set(
        chapter.segments
          .map((entry) => entry.figureId)
          .filter((figureId): figureId is string => figureId != null),
      ),
    ];
    if (wanted.length === 0) return;
    let cancelled = false;
    invoke<ReadingFigure[]>("get_reading_figures", {
      bookId: item.bookId,
      figureIds: wanted,
    })
      .then((payload) => {
        if (!cancelled) setFigures(payload);
      })
      .catch(() => {});
    return () => {
      cancelled = true;
    };
  }, [chapter, item.bookId]);

  useDialogFocus(panelRef, true, {
    onEscape: onClose,
    fallbackSelector: FOLLOW_ALONG_FALLBACK,
  });

  useEffect(() => {
    const onKeyDown = (event: KeyboardEvent) => {
      if (!isTopDialog(panelRef.current)) return;
      if (event.key === "ArrowLeft" || event.key === "ArrowRight") {
        const target = event.target as HTMLElement | null;
        if (
          event.defaultPrevented ||
          event.altKey ||
          event.ctrlKey ||
          event.metaKey ||
          target?.closest(
            "input, textarea, select, [contenteditable], [role=slider], [role=menu], [role=tablist]",
          ) != null
        ) {
          return;
        }
        const flip = flipRef.current;
        if (flip == null) return;
        event.preventDefault();
        const rtl =
          getComputedStyle(document.documentElement).direction === "rtl";
        const forward = (event.key === "ArrowRight") !== rtl;
        flip(forward ? 1 : -1);
        return;
      }
      if (event.key === "[") {
        const target = event.target as HTMLElement | null;
        const tag = target?.tagName;
        if (tag === "INPUT" || tag === "TEXTAREA" || target?.isContentEditable)
          return;
        event.preventDefault();
        if (focusModeRef.current) {
          setFocusMode(false);
          setOutlineOpen(true);
          storeOutlineOpen(true);
          return;
        }
        const next = !outlineOpenRef.current;
        setOutlineOpen(next);
        storeOutlineOpen(next);
      }
    };
    window.addEventListener("keydown", onKeyDown);
    return () => {
      window.removeEventListener("keydown", onKeyDown);
    };
  }, [onClose]);
  useEffect(() => {
    const onShowTutor = () => {
      setFocusMode(false);
      setCompanionTab("tutor");
      const dock = tutorDockRef.current;
      if (dock.mode === "floating" && dock.folded) {
        const next = { ...dock, folded: false };
        tutorDockRef.current = next;
        setTutorDock(next);
        storeDock(next);
      }
    };
    window.addEventListener(READER_SHOW_TUTOR_EVENT, onShowTutor);
    return () =>
      window.removeEventListener(READER_SHOW_TUTOR_EVENT, onShowTutor);
  }, []);

  // The 55%-of-viewport rail ceiling holds continuously, not just at load
  // and interaction time — a window shrink re-clamps the live width.
  useEffect(() => {
    const onWindowResize = () => {
      // Viewport width is state so the handle's aria-valuemax re-renders
      // even when the current width already fits the new bounds.
      setViewportWidth(window.innerWidth);
      setViewportHeight(window.innerHeight);
      const next = clampRailWidth(railWidthRef.current, window.innerWidth);
      if (next !== railWidthRef.current) {
        railWidthRef.current = next;
        setRailWidth(next);
      }
    };
    window.addEventListener("resize", onWindowResize);
    return () => window.removeEventListener("resize", onWindowResize);
  }, []);

  useEffect(() => {
    let cancelled = false;
    invoke<BookSummary[]>("list_books")
      .then((books) => {
        if (cancelled) return;
        setBook(books.find((entry) => entry.id === item.bookId) ?? null);
      })
      .catch(() => {});
    return () => {
      cancelled = true;
    };
  }, [item.bookId]);

  useEffect(() => {
    overlayInputs.current = { mode, pages, figures, isTeacher: isTeacherJob };
  }, [figures, isTeacherJob, mode, pages]);

  useEffect(() => {
    let seekNonce = getSnapshot().seekNonce;
    return subscribePlayback(() => {
      const next = getSnapshot().seekNonce;
      if (next === seekNonce) return;
      const advanced = seekNonceAdvanced(seekNonce, next);
      seekNonce = next;
      if (!advanced) return;
      dispatchFollow({ type: "seeked" });
    });
  }, [dispatchFollow]);

  const chapterId = chapter?.chapterId ?? null;
  useEffect(() => {
    figureHoldRef.current = null;
    followContextRef.current.narrationPage = null;
    dispatchFollow({ type: "chapterChanged" });
  }, [chapterId, dispatchFollow]);

  useEffect(() => {
    overlayApplier.reset();
    autoScroller.reset();
  }, [autoScroller, mode, overlayApplier]);

  useEffect(() => {
    const element = scrollRef.current;
    if (element == null) return;
    const measure = () => {
      setSurfaceBox((previous) =>
        previous.width === element.clientWidth &&
        previous.height === element.clientHeight
          ? previous
          : { width: element.clientWidth, height: element.clientHeight },
      );
    };
    measure();
    if (typeof ResizeObserver === "undefined") return;
    const observer = new ResizeObserver(measure);
    observer.observe(element);
    return () => observer.disconnect();
  }, [mode]);

  useEffect(() => {
    const sample = pageModeSample(mode, reason, chapter);
    if (!shouldEmitPageMode(lastPageMode.current, sample)) return;
    lastPageMode.current = sample;
    void reportPageMode(sample);
  }, [chapter, mode, reason]);

  const onFit = useCallback((next: PageFit) => {
    setFit(next);
    storePageFit(next);
  }, []);

  const onZoom = useCallback((next: number) => {
    const clamped = clampPageZoom(next);
    setZoom(clamped);
    storePageZoom(clamped);
  }, []);

  const onZoomIn = useCallback(() => onZoom(zoomInFrom(zoom)), [onZoom, zoom]);

  const onZoomOut = useCallback(
    () => onZoom(zoomOutFrom(zoom)),
    [onZoom, zoom],
  );

  // Citation pause fires ONLY while a citation is actually displayed or
  // latest-pending (round-4: ordinary seek/tap/scroll must stay exactly on
  // the tutor-campaign follow law — no citation, no citation state change).
  const pauseCitationIfLive = useCallback(() => {
    const c = citationRef.current;
    if ((c.shown != null || c.latest != null) && !c.paused) {
      dispatchCitation({ type: "manualScroll" });
    }
  }, [dispatchCitation]);
  const onManualScroll = useCallback(() => {
    autoScroller.manualScroll(followRef.current.mode === "following");
    // §11.1: a manual user scroll pauses anchor-following; sync never
    // fights the user's scroll. No-op while no anchor is active.
    if (!tutorGuidingRef.current) dispatchAnchor({ type: "pause" });
    // The Agent citation obeys the same law (charter A8/§7.2) — when live.
    pauseCitationIfLive();
  }, [autoScroller, dispatchAnchor, pauseCitationIfLive]);

  const onSurfaceScroll = useCallback(() => {
    if (checkpointValue.current != null)
      checkpointValue.current.scroll = scrollRef.current?.scrollTop ?? 0;
    // The scroll EVENT is a result, not a gesture: the reader's own scrolls
    // (the autoScroller's reset, the citation box's scrollIntoView) fire it
    // too. Only the applier reads it — it recognises its own expectedTop.
    // The citation pause stays on gestures: wheel/touch (onManualScroll),
    // keys (onSurfaceKeyDown), flips (onFlip) — so a Resume is never undone
    // by the scroll it caused (QA-DIAG-1 / AT-1-3; round-3 item 3 kept).
    autoScroller.scrolled(
      scrollRef.current,
      followRef.current.mode === "following",
    );
  }, [autoScroller]);

  const onSurfaceKeyDown = useCallback(
    (event: React.KeyboardEvent<HTMLDivElement>) => {
      if (isScrollKey(event.key)) onManualScroll();
    },
    [onManualScroll],
  );

  const onFlip = useCallback(
    (delta: 1 | -1) => {
      setHistoryBrowse(false);
      const base = shownPage(
        followRef.current,
        followContextRef.current.narrationPage,
      );
      const visible = tutorGuidingRef.current ? shownPageRef.current : base;
      dispatchFollow({
        type: "flip",
        delta: delta + (visible != null && base != null ? visible - base : 0),
      });
      // Flipping away is a manual scroll in the §11.1 sense too.
      dispatchAnchor({ type: "pause" });
      pauseCitationIfLive();
      // F8 D2 bidirectional (BookDirect only): a manual page turn moves the
      // session's BookCursor so the grounding window follows the page the
      // user sees. Grounded sessions never hear about it — playback
      // position is authoritative there and scroll must never seek audio.
      if (direct && tutorLiveRef.current) {
        const turned = followRef.current;
        if (turned.mode === "browsing") {
          invoke("voice_session_page_turned", {
            sessionId: voiceSession.sessionId,
            bookId: item.bookId,
            jobId: itemJobId,
            pageNo: turned.pageNo,
          }).catch(() => {});
        }
      }
    },
    [
      direct,
      dispatchAnchor,
      dispatchFollow,
      pauseCitationIfLive,
      voiceSession.sessionId,
      item.bookId,
      itemJobId,
    ],
  );
  const flipRef = useRef<((delta: 1 | -1) => void) | null>(null);
  flipRef.current = loadError == null ? onFlip : null;
  const onReturn = useCallback(() => {
    if (tutorGuidingRef.current && tutorReturnRef.current != null) {
      tutorReturnRef.current();
      return;
    }
    dispatchFollow({ type: "returnToNarration" });
    setHistoryBrowse(false);
    dispatchAnchor({ type: "resume" });
    // The Agent citation resumes through the same affordance (round-2 item
    // 6): resume re-shows the latest citation and following continues.
    dispatchCitation({ type: "resume" });
    autoScroller.returned();
  }, [autoScroller, dispatchAnchor, dispatchCitation, dispatchFollow]);
  const browseToPage = useCallback(
    (pageNo: number) => {
      const base = shownPage(
        followRef.current,
        followContextRef.current.narrationPage,
      );
      if (base == null) return;
      dispatchFollow({ type: "flip", delta: pageNo - base });
      dispatchAnchor({ type: "pause" });
      setHistoryBrowse(true);
    },
    [dispatchAnchor, dispatchFollow],
  );
  const openHistoryTarget = useCallback(
    (target: OpenTarget) => {
      if (target.kind === "page") {
        browseToPage(target.page);
        return;
      }
      if (target.kind !== "ms" || index == null || itemJobId == null) return;
      const position = chapterToLoadAt(index, target.ms);
      if (position == null) return;
      const chapterId = index.chapters[position].chapterId;
      const resolve = (chapter: ReadingChapter) => {
        const at = segmentAt(chapter, target.ms);
        if (at == null) return;
        const page = pageToShow(
          chapter.segments[at],
          overlayInputs.current.figures,
        );
        if (page != null) browseToPage(page);
      };
      const cached = cache.get(itemJobId, chapterId);
      if (cached != null) {
        resolve(cached);
        return;
      }
      invoke<ReadingChapter>("get_reading_chapter", {
        jobId: itemJobId,
        chapterId,
      })
        .then((chapter) => {
          chapterBackoff.succeeded(chapterId);
          cache.set(itemJobId, chapterId, chapter);
          resolve(chapter);
        })
        .catch(() => {});
    },
    [browseToPage, cache, chapterBackoff, index, itemJobId],
  );
  const onTap = useCallback((outcome: TapOutcome) => {
    void reportReadingTap(tapLine(outcome));
    if (outcome.hit != null) {
      dispatchPlayback({ type: "seekMs", ms: outcome.hit.startMs });
    }
  }, []);

  const selectMode = useCallback(
    (next: ReadingMode) => {
      if (direct) return; // BookSurface has no script surface to switch to
      if (next === mode) return;
      dispatchFollow({ type: "readingModeChanged" });
      storeReadingMode(next);
      setMode(next);
    },
    [direct, dispatchFollow, mode],
  );

  const summary =
    index == null || chapter == null
      ? null
      : (index.chapters.find(
          (entry) => entry.chapterId === chapter.chapterId,
        ) ?? null);
  const lastPage = direct
    ? (item.pageCount ?? 0)
    : index == null
      ? 0
      : index.chapters.reduce(
          (highest, entry) => Math.max(highest, entry.pageEnd),
          0,
        );
  followContextRef.current.lastPage = lastPage;
  const segment =
    chapter == null || segmentIdx == null
      ? null
      : (chapter.segments[segmentIdx] ?? null);
  // BookSurface: the session's navigation (not a narration cursor) is the
  // "narration" the follow model tracks; page 1 before the first event.
  const narrationPageNo =
    tutorSessionLive && sessionPage != null
      ? sessionPage
      : direct
        ? (sessionPage ?? 1)
        : (followContextRef.current.narrationPage ??
          pageToShow(segment, figures));
  if (direct) followContextRef.current.narrationPage = narrationPageNo;
  // The synced tutor anchor outranks narration for the shown page (§11.1);
  // no active anchor (or an unresolvable id) contributes nothing and the
  // page behaves exactly as before. A user browsing outranks both.
  const syncedAnchorTarget = anchorTarget(
    anchorState.synced,
    figures,
    chapter?.segments ?? [],
  );
  const anchorPageNo =
    tutorSessionLive &&
    voiceSession.navigation?.callId != null &&
    syncedAnchorTarget?.pageNo !== narrationPageNo
      ? null
      : anchorPage(syncedAnchorTarget);
  // Citation steering (§5.2: highlight for grounded answers — the citation
  // turns the page like the spotlight does; manual scroll pauses it and the
  // resume affordance returns). Anchor steering wins when both exist (A10:
  // one authority at a time; the tutor spotlight cannot run in Agent mode).
  const citationPageNo =
    mode === "page" && !citationState.paused && citationState.shown != null
      ? citationState.shown.page
      : null;
  const shownPageNo = shownPage(
    followState,
    anchorPageNo ?? citationPageNo ?? narrationPageNo,
  );
  shownPageRef.current = shownPageNo;
  const pageShownReport = tutorSessionLive
    ? shownPageNo
    : displayedPageReport(shownPageNo, narrationPageNo);
  const reportedPageShown = useRef<number | null | undefined>(undefined);
  useEffect(() => {
    if (!tutorSessionLive) return;
    const sessionId = voiceSession.sessionId;
    return () => {
      void invoke("voice_session_page_shown", {
        sessionId,
        bookId: item.bookId,
        jobId: itemJobId,
        pageNo: null,
      }).catch(() => {});
    };
  }, [tutorSessionLive, voiceSession.sessionId, item.bookId, itemJobId]);
  useEffect(() => {
    if (!tutorSessionLive) {
      reportedPageShown.current = undefined;
      return;
    }
    if (reportedPageShown.current === pageShownReport) return;
    reportedPageShown.current = pageShownReport;
    invoke("voice_session_page_shown", {
      sessionId: voiceSession.sessionId,
      bookId: item.bookId,
      jobId: itemJobId,
      pageNo: pageShownReport,
    }).catch(() => {});
  }, [
    pageShownReport,
    tutorSessionLive,
    voiceSession.sessionId,
    item.bookId,
    itemJobId,
  ]);
  const confirmedPage = useRef(checkpoint?.page ?? null);
  const navigationCheckpoint = useRef<NavigationCheckpoint>(
    checkpoint?.navigation ?? { consumed: 0, held: false, latest: null },
  );
  const saveNavigation = useCallback((value: NavigationCheckpoint) => {
    navigationCheckpoint.current = value;
  }, []);
  const tutorNavigation = useTutorNavigation({
    checkpoint:
      checkpoint?.sessionId === voiceSession.sessionId
        ? checkpoint.navigation
        : undefined,
    onCheckpoint: saveNavigation,
    sessionId: voiceSession.sessionId,
    bookId: item.bookId,
    live: tutorGuiding,
    navigation: voiceSession.navigation,
    outcome: voiceSession.navigationOutcome ?? null,
    pages,
    shownPage: shownPageNo,
    fetchPages: onNeedPages,
    apply: applyTutorNavigation,
    freeze: () => {
      const base = shownPage(
        followRef.current,
        followContextRef.current.narrationPage,
      );
      dispatchFollow({
        type: "flip",
        delta: shownPageNo != null && base != null ? shownPageNo - base : 0,
      });
      dispatchAnchor({ type: "pause" });
    },
  });
  const pageReady = tutorNavigation.pageReady;
  const confirmVisiblePage = useCallback(
    (pageNo: number) => {
      confirmedPage.current = pageNo;
      pageReady(pageNo);
    },
    [pageReady],
  );
  tutorReturnRef.current = tutorNavigation.goToTutor;
  const restoreScroll = useRef(checkpoint?.scroll ?? null);
  const checkpointValue = useRef<ReaderCheckpoint | null>(null);
  checkpointValue.current = {
    page: shownPageNo,
    scroll: scrollRef.current?.scrollTop ?? checkpoint?.scroll ?? 0,
    mode,
    fit,
    zoom,
    companionTab,
    focusMode,
    sessionId: voiceSession.sessionId,
    navigation: navigationCheckpoint.current,
  };
  const checkpointSink = useRef(onCheckpoint);
  checkpointSink.current = onCheckpoint;
  useEffect(
    () => () => {
      if (checkpointValue.current != null)
        checkpointSink.current?.({
          ...checkpointValue.current,
          page: confirmedPage.current,
          scroll:
            scrollRef.current?.scrollTop ?? checkpointValue.current.scroll,
          navigation: navigationCheckpoint.current,
        });
    },
    [],
  );
  useEffect(() => {
    const top = restoreScroll.current;
    const surface = scrollRef.current;
    if (
      top == null ||
      surface == null ||
      (top > 0 && surface.scrollHeight <= surface.clientHeight)
    )
      return;
    surface.scrollTop = top;
    restoreScroll.current = null;
  }, [shownPageNo, pages, mode]);

  modeRef.current = mode;
  segmentStartRef.current = segment?.startMs ?? 0;
  const direction: TravelDirection =
    followState.mode === "browsing" ? travelRef.current : 1;
  const surfacePage =
    shownPageNo == null ? null : (pages.get(shownPageNo) ?? null);
  // The page-mode telemetry keeps logging the flag-derived `reason` alongside
  // anchorGeometry; only the user-facing banner is chapter-aware.
  const advisory = direct
    ? null
    : pageModeAdvisory(item.mode, item.isScanned, chapter);
  const fallbackPlan: OverlayPlan =
    mode === "page" && !tutorSessionLive && !isTeacherJob
      ? overlayPlan(segment, null, shownPageNo, surfacePage, isTeacherJob)
      : NO_OVERLAY;
  const spotlight =
    mode === "page"
      ? spotlightPlan(
          syncedAnchorTarget,
          shownPageNo,
          surfacePage,
          spotlightEnabled,
        )
      : null;
  const citationPlan =
    mode === "page"
      ? agentCitationPlan(
          citationState.shown,
          chapter,
          shownPageNo,
          surfacePage,
        )
      : null;
  // The applied-effect verdict, AFTER the render commit, from the SAME plan
  // the overlay renders — never an approximation of it.
  useEffect(() => {
    const pending = pendingAckRef.current;
    if (pending == null) return;
    const isShown =
      citationState.shown != null &&
      citationState.shown.ackCallId === pending.ackCallId;
    const isLatestWhilePaused =
      citationState.paused &&
      citationState.latest != null &&
      citationState.latest.ackCallId === pending.ackCallId;
    if (!isShown && !isLatestWhilePaused) return;
    const verdict = resolveAck(
      isShown ? citationPlan : null,
      mode === "page",
      citationState.paused && !isShown,
      shownPageNo,
      pending.page,
      surfacePage != null && surfacePage.pageNo === shownPageNo,
    );
    if (!verdict.done) return;
    pendingAckRef.current = null;
    ackCitation(pending.ackCallId, verdict.applied, verdict.reason);
  }, [citationPlan, citationState, mode, shownPageNo, surfacePage]);
  // Scroll the committed citation into view once per sync, like the
  // spotlight (7b's other half: the page turned above; now the box scrolls).
  const citationScrolledNonce = useRef(0);
  const citationBoxRef = useRef<HTMLDivElement | null>(null);
  useEffect(() => {
    if (citationState.syncNonce === citationScrolledNonce.current) return;
    if (citationPlan == null) return;
    citationScrolledNonce.current = citationState.syncNonce;
    citationBoxRef.current?.scrollIntoView({
      behavior: "smooth",
      block: "center",
    });
  }, [citationState.syncNonce, citationPlan]);
  useEffect(() => {
    // Scroll the spotlight into view once per sync (anchor change/resume),
    // waiting until the box exists on the shown page.
    if (anchorState.syncNonce === scrolledSyncNonce.current) return;
    if (spotlight == null) return;
    scrolledSyncNonce.current = anchorState.syncNonce;
    spotlightOutlineRef.current?.scrollIntoView({
      block: "nearest",
      inline: "nearest",
    });
  });
  useEffect(() => {
    autoScroller.commitPage({
      container: scrollRef.current,
      word: overlayWordRef.current,
      envelope: overlayFallbackRef.current,
    });
  }, [autoScroller, followState, shownPageNo, surfaceBox, fit, zoom]);

  const fullBleed = loadError == null;
  const pageSurface = mode === "page" && loadError == null;
  const pageWidthPx = fittedWidthPx({
    containerWidth: surfaceBox.width - SURFACE_GUTTER_PX * 2,
    containerHeight: surfaceBox.height - SURFACE_GUTTER_PX * 2,
    ratio: aspectRatio(surfacePage),
    fit,
    zoom,
  });

  return createPortal(
    <div
      className={`animate-fade-in fixed inset-0 z-50 flex ${
        fullBleed ? "" : "items-center justify-center p-6"
      }`}
      data-testid="reading-view"
      data-job-id={itemJobId ?? ""}
      data-modal-layer=""
      data-surface={pageSurface ? "page" : "script"}
    >
      <button
        type="button"
        aria-label={t("reader.closeFollowAlong")}
        onClick={onClose}
        className="absolute inset-0 cursor-default bg-ink-950/95 backdrop-blur-md"
      />
      <ReadingThreshold />
      <div
        ref={panelRef}
        role="dialog"
        aria-modal="true"
        aria-label={t("reader.followAlongTitle", { title: item.title })}
        tabIndex={-1}
        className={
          fullBleed
            ? "relative flex h-full w-full bg-ink-950 outline-none"
            : "animate-modal-in shadow-overlay relative flex w-full max-w-3xl flex-col gap-6 rounded-2xl border border-ink-800/70 bg-ink-900 p-8 outline-none"
        }
      >
        {fullBleed && wideShell && !focusMode && (
          <ReadingOutline
            folded={!outlineOpen}
            index={index}
            title={item.title}
            currentChapterId={chapter?.chapterId ?? null}
            bookChapterCount={book?.chapterCount ?? null}
            onSeek={(target) =>
              dispatchPlayback({ type: "seekMs", ms: target.startMs })
            }
          />
        )}
        {fullBleed && wideShell && focusMode && (
          <button
            type="button"
            data-testid="focus-edge-handle"
            aria-label={t("reader.showChaptersAndCompanion")}
            onClick={() => setFocusMode(false)}
            className="flex w-6 shrink-0 items-center justify-center border-e border-ink-800 text-ink-500 transition-colors duration-200 hover:text-ink-200"
          >
            <ChevronIcon className="h-4 w-4" />
          </button>
        )}
        <div
          className={
            fullBleed ? "flex h-full min-w-0 flex-1 flex-col" : "contents"
          }
        >
          <div
            className={
              fullBleed
                ? "flex shrink-0 items-center gap-3 border-b border-ink-800/60 px-5 py-2.5"
                : "contents"
            }
          >
            {fullBleed && !focusMode && (
              <button
                type="button"
                data-testid="outline-toggle"
                aria-label={
                  outlineOpen
                    ? t("reader.hideChapters")
                    : t("reader.showChapters")
                }
                aria-expanded={outlineOpen}
                title={t("reader.chaptersShortcut", { key: "[" })}
                onClick={() =>
                  setOutlineOpen((open) => {
                    storeOutlineOpen(!open);
                    return !open;
                  })
                }
                className="rounded-lg p-1.5 text-ink-400 transition-colors duration-200 hover:text-ink-100 active:scale-[0.98]"
              >
                <ChevronIcon
                  className={`h-4 w-4 transition-transform duration-200 ${
                    outlineOpen ? "rotate-180" : ""
                  }`}
                />
              </button>
            )}
            <ReadingHeader
              title={item.title}
              chapter={summary}
              origin={index?.origin ?? null}
              mode={mode}
              onSelectMode={selectMode}
              onClose={onClose}
              compact={fullBleed}
              scriptAvailable={!direct}
              utilities={
                <>
                  {navigationControls}
                  {utilities}
                </>
              }
              controls={
                pageSurface ? (
                  <ReadingPageZoom
                    fit={fit}
                    zoom={zoom}
                    onFit={onFit}
                    onZoom={onZoom}
                    onZoomIn={onZoomIn}
                    onZoomOut={onZoomOut}
                  />
                ) : undefined
              }
            />
            {fullBleed && (
              <button
                type="button"
                data-testid="focus-toggle"
                aria-pressed={focusMode}
                onClick={() => setFocusMode((focused) => !focused)}
                className="shrink-0 rounded-lg border border-ink-800 px-3 py-1 text-secondary font-medium text-ink-300 transition-colors duration-200 hover:text-ink-100 active:scale-[0.98]"
              >
                {focusMode ? t("reader.exitFocus") : t("reader.focus")}
              </button>
            )}
          </div>
          {pageSurface && advisory && (
            <p
              data-testid="reading-page-advisory"
              className="shrink-0 border-b border-ink-800/60 px-5 py-2 text-label leading-snug text-reading-stale"
            >
              {advisory}
            </p>
          )}
          {notice}
          {loadError == null && <ReadingSuppressed records={suppressed} />}
          {engineError && (
            <p
              role="alert"
              data-testid="reading-engine-error"
              className={
                fullBleed
                  ? "shrink-0 px-5 py-1.5 text-sm text-reading-stale"
                  : "text-sm text-reading-stale"
              }
            >
              {engineError}
            </p>
          )}
          {loadError ? (
            <p
              role="alert"
              data-testid="reading-error"
              className="text-sm text-ink-300"
            >
              {loadError}
            </p>
          ) : (
            <div className="relative flex min-h-0 flex-1 flex-col">
              <div
                ref={pageSurface ? scrollRef : undefined}
                data-testid="reading-surface"
                data-mode={mode}
                data-fit={pageSurface ? fit : undefined}
                tabIndex={0}
                aria-label={
                  pageSurface ? t("reader.printedPage") : t("reader.script")
                }
                onScroll={pageSurface ? onSurfaceScroll : undefined}
                onWheel={pageSurface ? onManualScroll : undefined}
                onTouchMove={pageSurface ? onManualScroll : undefined}
                onKeyDown={pageSurface ? onSurfaceKeyDown : undefined}
                className="min-h-0 flex-1 overflow-auto overscroll-contain outline-none focus-visible:ring-1 focus-visible:ring-ember-500/40 focus-visible:ring-inset"
              >
                {pageSurface ? (
                  <div className="flex min-h-full items-start justify-center p-6">
                    <p
                      aria-live="polite"
                      data-testid="reading-page-live"
                      className="sr-only"
                    >
                      {segment?.text ?? ""}
                    </p>
                    <div
                      data-testid="reading-page-box"
                      style={
                        pageWidthPx != null
                          ? { width: `${pageWidthPx}px` }
                          : undefined
                      }
                      className={`${pageWidthPx == null ? "w-full max-w-[1500px]" : "shrink-0"} shadow-overlay`}
                    >
                      <ReadingFacsimile
                        segment={segment}
                        pageNo={shownPageNo}
                        direction={direction}
                        pages={pages}
                        figures={figures}
                        lastPage={lastPage}
                        onNeedPages={onNeedPages}
                        onPageReady={confirmVisiblePage}
                        readyToken={tutorNavigation.readyToken}
                        overlay={
                          <>
                            <ReadingSpotlight
                              plan={spotlight}
                              outlineRef={spotlightOutlineRef}
                            />
                            <AgentCitationOverlay
                              boxRef={citationBoxRef}
                              plan={citationPlan}
                              onDismiss={() => {
                                settlePendingAck("dismissed");
                                dispatchCitation({ type: "dismiss" });
                              }}
                            />
                            <ReadingPageOverlay
                              boxes={fallbackPlan.boxes}
                              wordRef={overlayWordRef}
                              fallbackRef={overlayFallbackRef}
                            />
                          </>
                        }
                        hitLayer={
                          <ReadingTapLayer
                            segments={chapter?.segments ?? []}
                            pageNo={shownPageNo}
                            page={surfacePage}
                            onTap={onTap}
                          />
                        }
                      />
                    </div>
                  </div>
                ) : (
                  <div className="flex min-h-full justify-center px-8 pt-[8vh] pb-10">
                    <div
                      data-testid="reading-column"
                      className="flex w-full max-w-shelf flex-col gap-12 sm:flex-row sm:items-start sm:justify-center"
                    >
                      <div className="min-w-0 max-w-[38rem] flex-1">
                        <ReadingScript
                          ref={scriptRef}
                          segment={segment}
                          language={book?.language}
                          underlineRef={null}
                          renderCount={renderCountRef.current}
                          onRepair={onRepair}
                        />
                      </div>
                      <div className="w-full shrink-0 sm:w-64">
                        <ReadingFacsimile
                          segment={segment}
                          pageNo={shownPageNo}
                          direction={direction}
                          pages={pages}
                          figures={figures}
                          lastPage={lastPage}
                          onNeedPages={onNeedPages}
                          onPageReady={confirmVisiblePage}
                          readyToken={tutorNavigation.readyToken}
                        />
                      </div>
                    </div>
                  </div>
                )}
              </div>
              <ReadingPageEdges
                disabled={shownPageNo == null}
                onFlip={onFlip}
              />
            </div>
          )}
          {!fullBleed && (
            <VoiceSessionPanel bookId={item.bookId} jobId={itemJobId} />
          )}
          {fullBleed && resumeNotice && (
            <div className="pointer-events-none absolute inset-x-0 bottom-24 z-10 flex justify-center">
              <p
                data-testid="session-resume-chip"
                className="animate-rise-in shadow-overlay pointer-events-auto flex items-center gap-2.5 rounded-md border border-ink-800 bg-ink-900 py-1.5 ps-4 pe-2 text-secondary text-ink-200"
              >
                {t("reader.resumed")}
                <button
                  type="button"
                  aria-label={t("reader.dismiss")}
                  onClick={onDismissResumeNotice}
                  className="rounded-full p-1 text-ink-400 transition-colors duration-200 hover:text-ink-100"
                >
                  ×
                </button>
              </p>
            </div>
          )}
          {fullBleed && !wideShell && !focusMode && (
            <VoiceSessionPanel
              bookId={item.bookId}
              jobId={itemJobId}
              onOpenTarget={openHistoryTarget}
            />
          )}
          {fullBleed ? (
            <div className="flex shrink-0 flex-col gap-2 border-t border-ink-800/60 px-5 py-2.5">
              <div className="flex items-center gap-5">
                {focusMode && (
                  <button
                    type="button"
                    data-testid="ask-tutor"
                    onClick={() => setFocusMode(false)}
                    className="flex shrink-0 items-center gap-1.5 rounded-lg border border-ember-500/40 px-3 py-1.5 text-secondary font-medium text-ember-300 transition-colors duration-200 hover:bg-ember-500/10 active:scale-[0.98]"
                  >
                    <SparkIcon className="h-4 w-4" /> {t("reader.askTutor")}
                  </button>
                )}
                {tutorGuiding ? (
                  <TutorNavigationControls
                    pdf={book?.format === "pdf"}
                    held={tutorNavigation.held}
                    latestPage={tutorNavigation.latest?.pageNo ?? null}
                    shownPage={shownPageNo}
                    onKeep={tutorNavigation.keepPage}
                    onReturn={tutorNavigation.goToTutor}
                  />
                ) : (
                  <ReadingPageControls
                    browsing={
                      followState.mode === "browsing" ||
                      anchorState.paused ||
                      citationState.paused
                    }
                    onReturn={onReturn}
                    resumeLabel={
                      historyBrowse
                        ? F11_STRINGS.historyReturnLabel
                        : anchorState.paused || citationState.paused
                          ? t("reader.followingOffResume")
                          : undefined
                    }
                  />
                )}
                {!direct && (
                  <div className="min-w-0 flex-1">
                    <PlayerCapsule chapters={index?.chapters ?? []} />
                  </div>
                )}
              </div>
              {!direct && <ReadingUnderline underlineRef={underlineRef} />}
            </div>
          ) : (
            !direct && <AudioPlayer variant="full" tone="page" />
          )}
        </div>
        {fullBleed && wideShell && !focusMode && (
          <StudyCompanion
            dock={
              tutorDock.mode === "floating"
                ? {
                    ...tutorDock,
                    ...clampFloat(
                      tutorDock,
                      floatSize.width,
                      floatSize.height,
                      {
                        width: viewportWidth,
                        height: viewportHeight,
                      },
                    ),
                  }
                : tutorDock
            }
            floatSize={floatSize}
            live={tutorSessionLive}
            onFloat={() =>
              applyTutorDock(
                defaultFloat(
                  { width: window.innerWidth, height: window.innerHeight },
                  floatSize.width,
                  getComputedStyle(document.documentElement).direction === "rtl"
                    ? "rtl"
                    : "ltr",
                ),
              )
            }
            onDock={() => applyTutorDock({ mode: "docked" })}
            onFold={(folded) => {
              const current = tutorDockRef.current;
              if (current.mode === "floating") {
                applyTutorDock({ ...current, folded });
              }
            }}
            onMove={(x, y) => {
              const current = tutorDockRef.current;
              if (current.mode !== "floating") return;
              const next = {
                ...current,
                ...clampFloat({ x, y }, floatSize.width, floatSize.height, {
                  width: window.innerWidth,
                  height: window.innerHeight,
                }),
              };
              tutorDockRef.current = next;
              setTutorDock(next);
            }}
            onMoveCommit={() => storeDock(tutorDockRef.current)}
            title={item.title}
            book={book}
            castChapters={index?.chapters.length ?? null}
            tab={companionTab}
            onSelectTab={setCompanionTab}
            width={railWidth}
            minWidth={RAIL_MIN_PX}
            maxWidth={railMaxPx(viewportWidth)}
            onResize={(px) => {
              const next = clampRailWidth(px, window.innerWidth);
              railWidthRef.current = next;
              setRailWidth(next);
            }}
            onResizeCommit={() => storeRailWidth(railWidthRef.current)}
            onResizeReset={() => {
              resetRailWidth();
              const next = clampRailWidth(RAIL_DEFAULT_PX, window.innerWidth);
              railWidthRef.current = next;
              setRailWidth(next);
            }}
            tutor={
              <VoiceSessionPanel
                variant="side"
                bookId={item.bookId}
                jobId={itemJobId}
                onOpenTarget={openHistoryTarget}
              />
            }
            bookAgent={
              <BookAgentPanel
                bookId={item.bookId}
                jobId={itemJobId}
                playbackPosMs={() =>
                  // The reading clock's own position (the audio-element-fed
                  // narration segment), never playbackStore.current — the
                  // clock-source pin owns that law.
                  Math.max(0, Math.round(segmentStartRef.current))
                }
                sessionLive={tutorSessionLive}
                session={
                  <VoiceSessionPanel
                    variant="side"
                    bookId={item.bookId}
                    jobId={itemJobId}
                    onOpenTarget={openHistoryTarget}
                  />
                }
                fallback={
                  <BookPanel
                    title={item.title}
                    book={book}
                    castChapters={index?.chapters.length ?? null}
                  />
                }
              />
            }
          />
        )}
      </div>
    </div>,
    document.body,
  );
}

export default ReadingView;
