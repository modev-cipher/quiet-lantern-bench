export interface StudioBook {
  id: string;
  title: string;
  author: string;
  meta: string;
  tone: string;
  availability: string;
  detail: string;
  position: string | null;
  cast: boolean;
  sendable: boolean;
  stale?: boolean;
}

export interface OrphanExport {
  title: string;
  meta: string;
  missing: boolean;
}

export interface StudioChapter {
  no: number;
  title: string;
  state: "ready" | "casting" | "none";
  current?: boolean;
}

export interface TutorTurn {
  from: "you" | "tutor";
  text: string;
  excerpt?: string;
}

export const PRIMARY_BOOK: StudioBook = {
  id: "primer",
  title: "Arabic Grammar Primer",
  author: "M. Haroun",
  meta: "96 pages · 7 chapters",
  tone: "bg-paper-base-a",
  availability: "Ready through Chapter 5",
  detail: "Casting Chapters 6–7 · about 12 min",
  position: "Chapter 3 · Verb Forms · 12:41",
  cast: true,
  sendable: true,
};

export const SHELF_BOOKS: StudioBook[] = [
  PRIMARY_BOOK,
  {
    id: "physics",
    title: "The Physics of Sound",
    author: "R. Ellison",
    meta: "312 pages · 9 chapters",
    tone: "bg-paper-base-d",
    availability: "Ready through Chapter 2",
    detail: "Chapters 3–9 queued behind the current cast",
    position: "Chapter 2 · Resonance · 03:57",
    cast: true,
    sendable: true,
  },
  {
    id: "rhetoric",
    title: "Letters on Rhetoric",
    author: "J. Alvarez",
    meta: "204 pages · 12 chapters",
    tone: "bg-paper-base-c",
    availability: "All 12 chapters ready",
    detail: "2 exports · Chapters 1–6 and 7–12 · finished yesterday",
    position: "Chapter 9 · Cadence · 41:08",
    cast: true,
    sendable: true,
  },
  {
    id: "andalus",
    title: "Poems of al-Andalus",
    author: "tr. S. Qureshi",
    meta: "148 pages · 6 sections",
    tone: "bg-paper-base-e",
    availability: "Ready through Section 4",
    detail: "Sections follow page ranges",
    position: "Section 3 · Córdoba · 22:34",
    cast: true,
    sendable: true,
  },
  {
    id: "tides",
    title: "A Field Guide to Tides",
    author: "H. Okonkwo",
    meta: "180 pages · 11 chapters",
    tone: "bg-paper-base-b",
    availability: "Not cast yet",
    detail: "Imported this morning",
    position: null,
    cast: false,
    sendable: false,
  },
  {
    id: "optics",
    title: "Notes on Optics",
    author: "L. Marchetti",
    meta: "228 pages · 8 chapters",
    tone: "bg-paper-base-b",
    availability: "Audio stale — re-render available",
    detail: "Edited after casting · exports for Chapters 1–4 and 5–8",
    position: "Chapter 2 · Refraction · 17:03",
    cast: true,
    sendable: false,
    stale: true,
  },
];

export const ORPHAN_EXPORTS: OrphanExport[] = [
  {
    title: "Early draft narration",
    meta: "MP3 · 38 MB · kept after its cast was deleted",
    missing: false,
  },
  {
    title: "Field recordings",
    meta: "audio file missing from disk",
    missing: true,
  },
];

export const PRIMER_CHAPTERS: StudioChapter[] = [
  { no: 1, title: "The Alphabet", state: "ready" },
  { no: 2, title: "Nouns and Articles", state: "ready" },
  { no: 3, title: "Verb Forms", state: "ready", current: true },
  { no: 4, title: "The Verbal Sentence", state: "ready" },
  { no: 5, title: "Particles", state: "ready" },
  { no: 6, title: "Derived Stems", state: "casting" },
  { no: 7, title: "Reading Passages", state: "none" },
];

export const READER_PARAGRAPHS: {
  text: string;
  highlight?: boolean;
}[] = [
  {
    text: "Every Arabic verb is built on a root of three consonants, and the pattern poured around that root carries the grammar: who acts, when, and how.",
  },
  {
    text: "The verbal sentence opens with its verb, and the subject follows it. A reader trained on English word order will want to reverse the two; resist the habit, because the verb-first shape is not an inversion here.",
  },
  {
    text: "This ordering is the default in classical prose.",
    highlight: true,
  },
  {
    text: "When the subject does come first, the sentence changes family entirely — it becomes a nominal sentence, and its verb, if one appears at all, behaves like a comment on the subject rather than the engine of the clause.",
  },
  {
    text: "Later chapters return to nominal sentences and the particles that govern them.",
  },
];

export const READER_PARAGRAPHS_AR: {
  text: string;
  highlight?: boolean;
}[] = [
  {
    text: "يُبنى كل فعل عربي على جذر من ثلاثة أحرف، والوزن الذي يُصاغ حول هذا الجذر يحمل النحو كله: مَن فعل، ومتى، وكيف.",
  },
  {
    text: "تبدأ الجملة الفعلية بفعلها، ويأتي الفاعل بعده. ومن اعتاد ترتيب الجملة الإنجليزية سيرغب في قلب الاثنين؛ فليقاوم هذه العادة، لأن تقديم الفعل هنا ليس قلبًا للترتيب.",
  },
  {
    text: "هذا الترتيب هو الأصل في النثر الفصيح.",
    highlight: true,
  },
  {
    text: "أما إذا تقدّم الاسم فقد انتقلت الجملة إلى أسرة أخرى تمامًا — صارت جملة اسمية، وفعلها، إن وُجد، يجري مجرى الخبر عن المبتدأ لا مجرى محرّك الجملة.",
  },
  {
    text: "وتعود الفصول اللاحقة إلى الجملة الاسمية والأدوات التي تحكمها.",
  },
];

export const TUTOR_TURNS: TutorTurn[] = [
  {
    from: "you",
    text: "Why does the verb come first in this sentence?",
    excerpt: "This ordering is the default in classical prose.",
  },
  {
    from: "tutor",
    text: "In Arabic, verbal sentences lead with the verb — the pattern is verb, then subject, then object. The line you selected is stating exactly that rule: classical prose treats verb-first as the unmarked, neutral order, not as emphasis.",
  },
  {
    from: "you",
    text: "So a sentence that starts with the noun is a different kind of sentence?",
  },
  {
    from: "tutor",
    text: "Yes — that is the nominal sentence the next paragraph introduces. Starting with the noun foregrounds it as a topic, and whatever follows is a comment about it. Chapter 4 works through both shapes side by side.",
  },
];
