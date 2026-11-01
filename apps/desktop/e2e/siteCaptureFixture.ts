import type { BookSummary } from "../src/libraryModel";
import type { ChapterSummary } from "../src/jobFormModel";
import type {
  MockCatalogModel,
  MockSessionRow,
  MockTranscriptTurn,
} from "./tauriMock";

export const BOOK_ID = "book-college-physics";
export const BOOK_TITLE = "College Physics 2e";
export const BOOK_AUTHOR = "OpenStax";
export const BOOK_LICENCE = "CC BY-NC-SA 4.0";
export const CAST_JOB_ID = "job-1";
export const CAST_EXPORT_ID = "export-job-1";
export const RUNNING_JOB_ID = "job-2";
export const TOTAL_MS = 3_328_000;
export const CAPTURE_SEEK_S = 1147;
export const PAGE_W = 612;
export const PAGE_H = 792;

export function captureBook(): BookSummary {
  return {
    id: BOOK_ID,
    title: BOOK_TITLE,
    author: BOOK_AUTHOR,
    pageCount: 1598,
    chapterCount: 34,
    language: "en",
    isScanned: false,
    coverPath: null,
    createdAt: 1_756_300_000_000,
    hasPseudoChapters: false,
    layoutAnalysed: true,
    reflowed: false,
    blocksStale: false,
  };
}

const CHAPTERS: Array<[string, number, number, number]> = [
  ["Introduction: The Nature of Science and Physics", 5, 34, 9_800],
  ["Kinematics", 35, 90, 18_400],
  ["Two-Dimensional Kinematics", 91, 128, 12_600],
  ["Dynamics: Force and Newton's Laws of Motion", 129, 170, 14_100],
  ["Further Applications of Newton's Laws", 171, 214, 15_300],
  ["Uniform Circular Motion and Gravitation", 215, 254, 13_200],
  ["Work, Energy, and Energy Resources", 255, 302, 16_700],
  ["Linear Momentum and Collisions", 303, 340, 12_900],
  ["Statics and Torque", 341, 376, 11_400],
  ["Rotational Motion and Angular Momentum", 377, 424, 15_800],
];

export function captureChapters(): ChapterSummary[] {
  return CHAPTERS.map(([title, pageStart, pageEnd, wordCount], idx) => ({
    id: `ch-${idx + 1}`,
    idx,
    title,
    pageStart,
    pageEnd,
    isPseudo: false,
    wordCount,
  }));
}

export function captureCatalogModels(): MockCatalogModel[] {
  return [
    {
      id: "studio-voice",
      task: "tts",
      display_name: "Studio voice",
      family: "studio-voice",
      quant: null,
      format: "pytorch",
      disk_bytes: 355_000_000,
      full_gpu_vram_mb: 1200,
      min_free_ram_mb: 2048,
      hybrid_capable: false,
      quality_score: 78,
      speed_score: 95,
      license: { spdx: "Apache-2.0", commercial_ok: true },
      perf: { rtf_gpu: 0.03, rtf_cpu: 1.0 },
    },
    {
      id: "teaching-model-9b",
      task: "llm",
      display_name: "Teaching model",
      family: "teaching",
      quant: null,
      format: "local",
      disk_bytes: 5_700_000_000,
      full_gpu_vram_mb: 7200,
      min_free_ram_mb: 8192,
      hybrid_capable: true,
      quality_score: 92,
      speed_score: 60,
      capabilities: ["teacher_mode", "figure_explanation"],
      license: { spdx: "Apache-2.0", commercial_ok: true },
    },
  ];
}

const MARGIN = 72;
const BODY_W = PAGE_W - MARGIN * 2;
const LINE_H = 15;
const CHAR_W = 5.15;
const LINE_CHARS = Math.floor(BODY_W / CHAR_W);

const PAGE_SECTIONS: Record<number, string> = {
  140: "4.2 Newton's First Law of Motion: Inertia",
  141: "4.3 Newton's Second Law of Motion: Concept of a System",
  142: "4.3 Newton's Second Law of Motion: Concept of a System",
  143: "4.4 Newton's Third Law of Motion: Symmetry in Forces",
};

const PROSE: string[] = [
  "A force is a push or a pull, and every force has both a size and a direction. Because of this, forces add as vectors rather than as plain numbers, and the sum of all forces acting on an object is called the net external force.",
  "Newton's first law states that a body at rest remains at rest, and a body in motion continues to move at constant velocity, unless acted upon by a net external force. The tendency of a body to resist a change in its motion is called inertia, and mass is its measure.",
  "Newton's second law makes the connection quantitative. The acceleration of a system is directly proportional to, and in the same direction as, the net external force acting on it, and inversely proportional to its mass. In symbols, the net force equals mass times acceleration.",
  "The word system is doing real work in that sentence. Before applying the second law you must decide what is inside the system and what is outside it, because only forces from outside the boundary can change the system's motion. Internal forces cancel in pairs.",
  "Consider a child pulling a sled across packed snow. The rope tension acts forward, friction acts backward, gravity acts downward, and the snow pushes upward. Only the forward and backward forces are unbalanced, so the sled accelerates along the ground and not into it.",
  "Weight is the force of gravity on an object, and near Earth's surface it equals the mass multiplied by the acceleration due to gravity. Mass is the same everywhere, but weight changes with the local gravitational field, which is why astronauts float yet still have inertia.",
  "Newton's third law completes the picture. Whenever one body exerts a force on a second body, the second exerts a force of equal magnitude and opposite direction on the first. The two forces act on different bodies, so they never cancel each other out.",
  "A swimmer pushes backward on the water and the water pushes the swimmer forward. A rocket pushes hot gas out of its nozzle and the gas pushes the rocket the other way. In each case the pair of forces is symmetric, but the resulting motions are not, because the masses differ.",
  "Free-body diagrams turn these ideas into a method. Draw the system as a point, sketch every external force as an arrow from that point, choose axes along the expected motion, and sum the components. The second law then yields the acceleration one axis at a time.",
  "Units follow from the law itself. One newton is the force that gives a one kilogram mass an acceleration of one metre per second squared. An apple weighs roughly one newton, which makes the unit easy to feel in the hand.",
  "Problems in this chapter reward patience more than algebra. Most mistakes come from forgetting a force, counting an internal force as external, or mixing up the body a force acts on. Slow down at the diagram stage and the equations tend to take care of themselves.",
  "Check your answer against intuition at the end. If a heavier sled comes out with a larger acceleration for the same pull, or if friction appears to speed something up, the diagram has a sign error somewhere, and finding it is where the physics is learned.",
];

const PAGE_START_PARAGRAPH: Record<number, number> = {
  140: 0,
  141: 2,
  142: 6,
  143: 9,
};

interface Line {
  text: string;
  y: number;
  chars: number;
  width: number;
}

interface Paragraph {
  index: number;
  lines: Line[];
}

function wrap(text: string): string[] {
  const words = text.split(" ");
  const lines: string[] = [];
  let current = "";
  for (const word of words) {
    const next = current.length === 0 ? word : `${current} ${word}`;
    if (next.length > LINE_CHARS && current.length > 0) {
      lines.push(current);
      current = word;
    } else {
      current = next;
    }
  }
  if (current.length > 0) lines.push(current);
  return lines;
}

export function pageParagraphs(pageNo: number): Paragraph[] {
  const start = PAGE_START_PARAGRAPH[pageNo] ?? pageNo % PROSE.length;
  const out: Paragraph[] = [];
  let y = MARGIN + 48;
  let index = start;
  while (y + LINE_H * 3 < PAGE_H - MARGIN - 24 && out.length < 5) {
    const text = PROSE[index % PROSE.length];
    const lines = wrap(text).map((line, at, all) => {
      const last = at === all.length - 1;
      const row: Line = {
        text: line,
        y: y + at * LINE_H,
        chars: line.length,
        width: last ? Math.min(BODY_W, line.length * CHAR_W) : BODY_W,
      };
      return row;
    });
    out.push({ index: index % PROSE.length, lines });
    y += lines.length * LINE_H + 11;
    index += 1;
  }
  return out;
}

function escapeXml(text: string): string {
  return text
    .replace(/&/g, "&amp;")
    .replace(/</g, "&lt;")
    .replace(/>/g, "&gt;")
    .replace(/'/g, "&apos;");
}

export function paragraphRect(pageNo: number, paragraphIndex: number) {
  const paragraph = pageParagraphs(pageNo).find(
    (entry) => entry.index === paragraphIndex,
  );
  if (paragraph == null) return null;
  const first = paragraph.lines[0];
  const last = paragraph.lines[paragraph.lines.length - 1];
  return {
    x0: MARGIN,
    y0: first.y - 11,
    x1: MARGIN + BODY_W,
    y1: last.y + 4,
  };
}

export function pageSvgDataUrl(pageNo: number): string {
  const section = PAGE_SECTIONS[pageNo] ?? "4.1 Development of Force Concept";
  const paragraphs = pageParagraphs(pageNo);
  const body = paragraphs
    .flatMap((paragraph) => paragraph.lines)
    .map(
      (line) =>
        `<text x="${MARGIN}" y="${line.y}" textLength="${line.width.toFixed(1)}" lengthAdjust="spacingAndGlyphs">${escapeXml(line.text)}</text>`,
    )
    .join("");
  const svg =
    `<svg xmlns="http://www.w3.org/2000/svg" width="${PAGE_W}" height="${PAGE_H}" viewBox="0 0 ${PAGE_W} ${PAGE_H}" font-family="Georgia, 'Times New Roman', serif" font-size="10.5" fill="#2a2622">` +
    `<rect width="${PAGE_W}" height="${PAGE_H}" fill="#fbf9f4"/>` +
    `<text x="${MARGIN}" y="${MARGIN + 6}" font-size="14" font-weight="600" fill="#1c1a17">${escapeXml(section)}</text>` +
    `<line x1="${MARGIN}" y1="${MARGIN + 18}" x2="${PAGE_W - MARGIN}" y2="${MARGIN + 18}" stroke="#c9661f" stroke-width="1.2"/>` +
    body +
    `<text x="${MARGIN}" y="${PAGE_H - 40}" font-size="8.5" fill="#6b655c">Chapter 4 | Dynamics: Force and Newton&apos;s Laws of Motion</text>` +
    `<text x="${PAGE_W - MARGIN}" y="${PAGE_H - 40}" font-size="9" fill="#2a2622" text-anchor="end">${pageNo}</text>` +
    `</svg>`;
  return `data:image/svg+xml;base64,${Buffer.from(svg).toString("base64")}`;
}

interface Spoken {
  pageNo: number;
  paragraph: number;
  startMs: number;
  text: string;
}

const SPOKEN: Spoken[] = [
  {
    pageNo: 140,
    paragraph: 1,
    startMs: 1_101_000,
    text: "Before the equations, hold on to one picture: nothing changes its motion on its own. A rolling ball slows because the floor drags on it, not because motion wears out.",
  },
  {
    pageNo: 141,
    paragraph: 2,
    startMs: 1_128_000,
    text: "So when the book says the acceleration is proportional to the net force, it is making a promise about the whole system, not about any single push.",
  },
  {
    pageNo: 141,
    paragraph: 3,
    startMs: 1_143_500,
    text: "Here is the trap almost everyone falls into: the word system means you decide where the boundary goes before you count a single force.",
  },
  {
    pageNo: 141,
    paragraph: 4,
    startMs: 1_160_000,
    text: "Picture the sled again. Tension pulls forward, friction drags backward, and the two vertical forces cancel, so only the horizontal pair decides how fast it speeds up.",
  },
  {
    pageNo: 142,
    paragraph: 6,
    startMs: 1_182_000,
    text: "Now the third law. Every force is one half of a pair, and the two halves always land on different bodies, which is exactly why they never cancel.",
  },
  {
    pageNo: 142,
    paragraph: 8,
    startMs: 1_205_000,
    text: "A free-body diagram is not decoration. It is the step where you choose the system, and every later equation inherits that choice.",
  },
];

function wordsOf(spoken: Spoken, endMs: number) {
  const tokens = spoken.text.split(" ");
  const total = tokens.reduce((sum, token) => sum + token.length + 1, 0);
  let cursor = spoken.startMs;
  const span = endMs - spoken.startMs;
  return tokens.map((token) => {
    const slice = ((token.length + 1) / total) * span;
    const word = {
      w: token,
      startMs: Math.round(cursor),
      endMs: Math.round(cursor + slice * 0.86),
      rect: null,
    };
    cursor += slice;
    return word;
  });
}

const CAST_CHAPTERS = [
  {
    chapterId: "cast-ch-4",
    idx: 0,
    title: "Dynamics: Force and Newton's Laws of Motion",
    pageStart: 129,
    pageEnd: 170,
    startMs: 0,
    endMs: 1_992_000,
  },
  {
    chapterId: "cast-ch-5",
    idx: 1,
    title: "Further Applications of Newton's Laws",
    pageStart: 171,
    pageEnd: 214,
    startMs: 1_992_000,
    endMs: TOTAL_MS,
  },
];

function castSegments() {
  return SPOKEN.map((spoken, idx) => {
    const endMs = (SPOKEN[idx + 1]?.startMs ?? spoken.startMs + 14_000) - 900;
    const rect = paragraphRect(spoken.pageNo, spoken.paragraph);
    return {
      segmentId: `seg-${String(idx).padStart(4, "0")}`,
      idx,
      pageNo: spoken.pageNo,
      kind: "text",
      figureId: null,
      text: spoken.text,
      startMs: spoken.startMs,
      endMs,
      provenance: "fresh",
      words: wordsOf(spoken, endMs),
      anchor: "block",
      rects: rect == null ? [] : [rect],
    };
  });
}

function pageRows(withImages: boolean) {
  const rows = [];
  for (let pageNo = 129; pageNo <= 214; pageNo += 1) {
    const rendered = pageNo >= 139 && pageNo <= 144;
    rows.push({
      pageNo,
      widthPt: PAGE_W,
      heightPt: PAGE_H,
      imagePath: rendered ? `<cache>/pa/college-physics-${pageNo}` : null,
      ...(withImages && rendered
        ? { imageDataUrl: pageSvgDataUrl(pageNo) }
        : {}),
    });
  }
  return rows;
}

export function captureReading() {
  const segments = castSegments();
  return {
    index: {
      jobId: CAST_JOB_ID,
      bookId: BOOK_ID,
      origin: "measured",
      totalMs: TOTAL_MS,
      chapters: CAST_CHAPTERS.map((chapter) => ({
        ...chapter,
        segmentCount: chapter.idx === 0 ? segments.length : 0,
      })),
    },
    chapters: [
      {
        anchorGeometry: "ensured",
        chapterId: "cast-ch-4",
        startMs: 0,
        endMs: 1_992_000,
        origin: "measured",
        segments,
      },
      {
        anchorGeometry: "ensured",
        chapterId: "cast-ch-5",
        startMs: 1_992_000,
        endMs: TOTAL_MS,
        origin: "measured",
        segments: [],
      },
    ],
    pages: pageRows(true),
    rendered: pageRows(false),
    figures: [],
  };
}

export const CITATION = {
  chapter: 0,
  page: 141,
  segmentIdx: 2,
};

const DAY = 86_400_000;
const NOW = 1_788_350_000_000;

export function captureSessions(): {
  rows: MockSessionRow[];
  turns: Record<string, MockTranscriptTurn[]>;
} {
  const rows: MockSessionRow[] = [
    {
      id: "0192b1c4-6a2e-7d1b-8f3a-1c2d3e4f5a61",
      book_id: BOOK_ID,
      job_id: CAST_JOB_ID,
      started_at: NOW - DAY - 3_600_000,
      ended_at: NOW - DAY - 2_460_000,
      end_reason: "ended",
      duration_ms: 1_140_000,
      turn_count: 9,
      summary_excerpt:
        "Worked out why the net force, not the rope alone, sets the sled's acceleration.",
      position: { kind: "ms", value: 1_160_000 },
      job_resolves: true,
      provider_changed: false,
    },
    {
      id: "0192b1c4-9c7f-7e2a-9b4c-2d3e4f5a6b72",
      book_id: BOOK_ID,
      job_id: CAST_JOB_ID,
      started_at: NOW - 3 * DAY,
      ended_at: NOW - 3 * DAY + 420_000,
      end_reason: "ended",
      duration_ms: 420_000,
      turn_count: 4,
      summary_excerpt: "Cleared up the difference between mass and weight.",
      position: { kind: "ms", value: 1_182_000 },
      job_resolves: true,
      provider_changed: false,
    },
  ];
  const turns: Record<string, MockTranscriptTurn[]> = {
    [rows[0].id]: [
      {
        turn_idx: 0,
        role: "user",
        text: "Why does the sled speed up if the snow pushes back just as hard?",
        created_at: rows[0].started_at + 20_000,
      },
      {
        turn_idx: 1,
        role: "assistant",
        text: "The snow's push is vertical and cancels gravity. Only the horizontal forces are unbalanced, and that leftover is what accelerates the sled.",
        created_at: rows[0].started_at + 32_000,
      },
    ],
    [rows[1].id]: [
      {
        turn_idx: 0,
        role: "user",
        text: "Is weight just another word for mass?",
        created_at: rows[1].started_at + 10_000,
      },
      {
        turn_idx: 1,
        role: "assistant",
        text: "No. Mass measures inertia and never changes. Weight is gravity's pull on that mass, so it depends on where you are.",
        created_at: rows[1].started_at + 24_000,
      },
    ],
  };
  return { rows, turns };
}

export const LIVE_TURNS: Array<{ role: "user" | "assistant"; text: string }> = [
  {
    role: "user",
    text: "Why do the two vertical forces just disappear from the problem?",
  },
  {
    role: "assistant",
    text: "They don't disappear, they cancel. The snow pushes up exactly as hard as gravity pulls down, so the vertical net force is zero and nothing accelerates that way.",
  },
  { role: "user", text: "So which force actually decides the acceleration?" },
  {
    role: "assistant",
    text: "The leftover horizontal one. Look at the passage on the page: tension forward, friction backward, and the difference between them, divided by the mass, is the acceleration.",
  },
];

export function captureVoiceProfiles() {
  return [
    {
      id: "preset-studio-heart",
      name: "Heart · American English (female)",
      engine: "studio-voice",
      kind: "preset",
      presetVoiceId: "af_heart",
    },
    {
      id: "preset-studio-michael",
      name: "Michael · American English (male)",
      engine: "studio-voice",
      kind: "preset",
      presetVoiceId: "am_michael",
    },
  ];
}

export function captureVoiceKeys() {
  return {
    entries: [
      {
        baseUrl: "https://generativelanguage.googleapis.com",
        provider: "gemini",
        last4: "k7Qd",
        state: "active" as const,
        keyPresent: true,
        custom: false,
      },
    ],
  };
}

export const CAST_PATH = "/exports/College Physics 2e - ch4-5 - teacher.mp3";
export const MEDIA_URL = "http://localhost:1421/site-capture-cast.wav";

export function captureJobs() {
  return [
    {
      id: CAST_JOB_ID,
      bookId: BOOK_ID,
      scope: { kind: "chapters", chapter_ids: ["ch-4", "ch-5"] },
      mode: "teacher",
      voiceProfileId: "preset-studio-heart",
      outputFormat: "mp3",
      modelOverridesJson: null,
      state: "DONE",
      overnight: false,
      verify: false,
      etaSeconds: null,
      reportJson: null,
      outputPathsJson: JSON.stringify([CAST_PATH]),
      createdAt: NOW - 2 * DAY,
      updatedAt: NOW - 2 * DAY + 3_727_000,
    },
    {
      id: RUNNING_JOB_ID,
      bookId: BOOK_ID,
      scope: { kind: "chapters", chapter_ids: ["ch-6", "ch-7"] },
      mode: "teacher",
      voiceProfileId: "preset-studio-heart",
      outputFormat: "mp3",
      modelOverridesJson: null,
      state: "SCRIPTING",
      overnight: false,
      verify: false,
      etaSeconds: 1380,
      reportJson: null,
      outputPathsJson: null,
      createdAt: NOW - 1_260_000,
      updatedAt: NOW - 4_000,
    },
  ];
}

export const RUNNING_PROGRESS = {
  job_id: RUNNING_JOB_ID,
  stage: "SCRIPTING",
  pct: 0.62,
  eta_seconds: 1380,
  current_item:
    "Explaining Figure 6.12, the banked curve, in the lecture script",
};

export function captureExports() {
  return [
    {
      id: CAST_EXPORT_ID,
      bookId: BOOK_ID,
      jobId: CAST_JOB_ID,
      path: CAST_PATH,
      bytes: 79_872_000,
      format: "mp3",
      mode: "teach",
      stale: false,
      createdAt: NOW - 2 * DAY + 3_727_000,
    },
  ];
}

export function longWav(durationS = TOTAL_MS / 1000, rate = 3000): Buffer {
  const frames = Math.round(durationS * rate);
  const bytes = Buffer.alloc(44 + frames);
  bytes.write("RIFF", 0);
  bytes.writeUInt32LE(36 + frames, 4);
  bytes.write("WAVE", 8);
  bytes.write("fmt ", 12);
  bytes.writeUInt32LE(16, 16);
  bytes.writeUInt16LE(1, 20);
  bytes.writeUInt16LE(1, 22);
  bytes.writeUInt32LE(rate, 24);
  bytes.writeUInt32LE(rate, 28);
  bytes.writeUInt16LE(1, 32);
  bytes.writeUInt16LE(8, 34);
  bytes.write("data", 36);
  bytes.writeUInt32LE(frames, 40);
  for (let frame = 0; frame < frames; frame += 1) {
    const seconds = frame / rate;
    const tone = Math.sin(2 * Math.PI * 180 * seconds) * 0.18;
    bytes[44 + frame] = Math.round(128 + tone * 127);
  }
  return bytes;
}
