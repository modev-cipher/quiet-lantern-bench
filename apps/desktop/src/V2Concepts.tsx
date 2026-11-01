import {
  AudioIcon,
  CastIcon,
  ChevronIcon,
  CloseIcon,
  JobsIcon,
  LibraryIcon,
  MicIcon,
  ModelsIcon,
  PlayIcon,
  SettingsIcon,
  SparkIcon,
  UploadIcon,
} from "./Icons";

const BOOKS = [
  {
    title: "Arabic Grammar Primer",
    author: "M. Haroun",
    meta: "96 pages · 7 chapters",
    tone: "bg-paper-base-a",
    status: "Casting · Synthesizing · 43%",
  },
  {
    title: "A Second, Much Longer Book Title That Wraps",
    author: "Another Author",
    meta: "340 pages · 18 chapters",
    tone: "bg-paper-base-c",
    status: "2 audio ready",
  },
  {
    title: "Fixture Book Title",
    author: "Fixture Author",
    meta: "12 pages · 3 chapters",
    tone: "bg-paper-base-d",
    status: "Imported",
  },
];

function Cover({
  book,
  className,
}: {
  book: (typeof BOOKS)[number];
  className?: string;
}) {
  return (
    <div
      className={`flex flex-col justify-end rounded-lg ${book.tone} p-4 ${className ?? ""}`}
    >
      <p className="font-serif text-heading font-semibold text-paper-fg">
        {book.title}
      </p>
      <p className="mt-1 font-serif text-secondary italic text-paper-fg-muted">
        {book.author}
      </p>
      <span className="mt-2 h-0.5 w-8 bg-ember-500" />
    </div>
  );
}

function StageBar() {
  return (
    <div className="flex items-stretch gap-0">
      <button
        type="button"
        className="flex min-w-[220px] flex-col gap-1 rounded-l-xl border border-ink-800 bg-ink-800 px-5 py-3 text-left"
      >
        <span className="flex items-center gap-2 text-body font-semibold text-ink-50">
          <LibraryIcon className="h-5 w-5 text-ember-400" /> Books
        </span>
        <span className="text-secondary text-ink-300">3 imported</span>
      </button>
      <span className="flex items-center border-y border-ink-800 bg-ink-900 px-2 text-ink-500">
        <ChevronIcon className="h-4 w-4" />
      </span>
      <button
        type="button"
        className="flex min-w-[300px] flex-col gap-1 border-y border-ink-800 bg-ink-900 px-5 py-3 text-left"
      >
        <span className="flex items-center justify-between gap-2">
          <span className="flex items-center gap-2 text-body font-semibold text-ink-200">
            <JobsIcon className="h-5 w-5" /> Casts
          </span>
          <span className="font-mono text-label text-ember-400">43%</span>
        </span>
        <span className="truncate text-secondary text-ink-300">
          Synthesizing · Arabic Grammar Primer
        </span>
        <span className="h-1 w-full rounded-full bg-ink-800">
          <span className="block h-1 w-[43%] rounded-full bg-ember-500" />
        </span>
      </button>
      <span className="flex items-center border-y border-ink-800 bg-ink-900 px-2 text-ink-500">
        <ChevronIcon className="h-4 w-4" />
      </span>
      <button
        type="button"
        className="flex min-w-[220px] flex-col gap-1 rounded-r-xl border border-ink-800 bg-ink-900 px-5 py-3 text-left"
      >
        <span className="flex items-center gap-2 text-body font-semibold text-ink-200">
          <AudioIcon className="h-5 w-5" /> Listen
        </span>
        <span className="text-secondary text-ink-300">
          2 ready · <span className="text-ember-400">1 new</span>
        </span>
      </button>
    </div>
  );
}

function ConceptSignalChain() {
  return (
    <div className="flex h-screen flex-col bg-ink-950 text-ink-100">
      <header className="flex items-center gap-6 border-b border-ink-800 bg-ink-900 px-6 py-4">
        <span className="flex items-center gap-3">
          <span className="flex h-10 w-10 items-center justify-center rounded-xl bg-ember-500/15 ring-1 ring-ember-500/25">
            <span className="font-serif text-xl font-semibold text-ember-300">
              t
            </span>
          </span>
        </span>
        <div className="flex flex-1 justify-center">
          <StageBar />
        </div>
        <div className="flex items-center gap-2">
          <button
            type="button"
            aria-label="Models"
            className="rounded-lg p-2.5 text-ink-400 hover:bg-ink-800"
          >
            <ModelsIcon className="h-5 w-5" />
          </button>
          <button
            type="button"
            aria-label="Settings"
            className="rounded-lg p-2.5 text-ink-400 hover:bg-ink-800"
          >
            <SettingsIcon className="h-5 w-5" />
          </button>
          <span className="ml-2 flex items-center gap-2 rounded-full bg-ink-850 px-3 py-1.5 text-label text-ink-300">
            <span className="h-2 w-2 rounded-full bg-wash-green-ink" /> Engine
            ready
          </span>
        </div>
      </header>
      <main className="flex flex-1 gap-10 overflow-hidden px-10 py-8">
        <section className="flex-1">
          <div className="flex items-end justify-between">
            <div>
              <h1 className="font-serif text-display font-semibold text-ink-50">
                Books
              </h1>
              <p className="mt-1 text-body text-ink-300">
                3 books ready to cast
              </p>
            </div>
            <button
              type="button"
              className="flex items-center gap-2 rounded-xl bg-ink-50 px-4 py-2.5 text-body font-semibold text-ink-950"
            >
              <UploadIcon className="h-5 w-5" /> Import book
            </button>
          </div>
          <div className="mt-8 grid grid-cols-[repeat(auto-fill,minmax(min(220px,100%),1fr))] gap-6">
            {BOOKS.map((book) => (
              <div key={book.title} className="flex flex-col gap-3">
                <Cover book={book} className="aspect-[3/4]" />
                <div className="flex items-center justify-between">
                  <span className="font-mono text-label text-ink-400">
                    {book.meta}
                  </span>
                  <button
                    type="button"
                    className="flex items-center gap-1.5 rounded-lg bg-ink-800 px-3 py-1.5 text-secondary font-semibold text-ink-100"
                  >
                    <CastIcon className="h-4 w-4" /> Cast
                  </button>
                </div>
              </div>
            ))}
          </div>
        </section>
        <aside className="flex w-[300px] shrink-0 flex-col gap-4 border-l border-ink-800 pl-8">
          <p className="text-label font-semibold tracking-[0.14em] text-ink-500 uppercase">
            Next in the chain
          </p>
          <div className="rounded-xl bg-ink-900 p-4">
            <p className="text-body font-semibold text-ink-100">
              Arabic Grammar Primer is casting
            </p>
            <p className="mt-1 text-secondary text-ink-300">
              Synthesizing · about 12 min left on your machine
            </p>
          </div>
          <div className="rounded-xl bg-ink-900 p-4">
            <p className="text-body font-semibold text-ink-100">
              1 new cast to hear
            </p>
            <p className="mt-1 text-secondary text-ink-300">
              Fixture Book Title finished yesterday. Play it, follow along, or
              send it to your phone.
            </p>
          </div>
          <div className="mt-auto rounded-xl bg-ink-850 p-4 text-secondary text-ink-300">
            You need to… run your first cast
          </div>
        </aside>
      </main>
    </div>
  );
}

function RailIcon({
  label,
  active,
  children,
}: {
  label: string;
  active?: boolean;
  children: React.ReactNode;
}) {
  return (
    <button
      type="button"
      aria-label={label}
      title={label}
      className={`flex h-11 w-11 items-center justify-center rounded-xl ${
        active ? "bg-ink-800 text-ember-400" : "text-ink-400 hover:bg-ink-800"
      }`}
    >
      {children}
    </button>
  );
}

function IconRail() {
  return (
    <nav className="flex w-[68px] shrink-0 flex-col items-center gap-1.5 border-r border-ink-800 bg-ink-900 py-5">
      <span className="mb-4 flex h-10 w-10 items-center justify-center rounded-xl bg-ember-500/15 ring-1 ring-ember-500/25">
        <span className="font-serif text-xl font-semibold text-ember-300">
          t
        </span>
      </span>
      <RailIcon label="Books" active>
        <LibraryIcon className="h-5 w-5" />
      </RailIcon>
      <RailIcon label="Audio">
        <AudioIcon className="h-5 w-5" />
      </RailIcon>
      <RailIcon label="Casts">
        <JobsIcon className="h-5 w-5" />
      </RailIcon>
      <RailIcon label="Models">
        <ModelsIcon className="h-5 w-5" />
      </RailIcon>
      <span className="mt-auto" />
      <RailIcon label="Settings">
        <SettingsIcon className="h-5 w-5" />
      </RailIcon>
    </nav>
  );
}

function ConceptControlRoom() {
  return (
    <div className="flex h-screen flex-col bg-ink-950 text-ink-100">
      <div className="flex min-h-0 flex-1">
        <IconRail />
        <main className="flex-1 overflow-hidden px-10 py-8">
          <div className="flex items-end justify-between">
            <div>
              <h1 className="font-serif text-display font-semibold text-ink-50">
                Books
              </h1>
              <p className="mt-1 text-body text-ink-300">
                3 books ready to cast
              </p>
            </div>
            <button
              type="button"
              className="flex items-center gap-2 rounded-xl bg-ink-50 px-4 py-2.5 text-body font-semibold text-ink-950"
            >
              <UploadIcon className="h-5 w-5" /> Import book
            </button>
          </div>
          <div className="mt-8 grid grid-cols-[repeat(auto-fill,minmax(min(220px,100%),1fr))] gap-6">
            {BOOKS.map((book) => (
              <div key={book.title} className="flex flex-col gap-3">
                <Cover book={book} className="aspect-[3/4]" />
                <div className="flex items-center justify-between">
                  <span className="font-mono text-label text-ink-400">
                    {book.meta}
                  </span>
                  <button
                    type="button"
                    className="flex items-center gap-1.5 rounded-lg bg-ink-800 px-3 py-1.5 text-secondary font-semibold text-ink-100"
                  >
                    <CastIcon className="h-4 w-4" /> Cast
                  </button>
                </div>
              </div>
            ))}
          </div>
        </main>
      </div>
      <footer className="grid grid-cols-[1.2fr_1fr_auto] items-center gap-8 border-t border-ink-800 bg-paper-base-e px-8 py-4">
        <div className="flex items-center gap-4">
          <span className="flex h-14 w-11 items-end rounded bg-paper-tone-c p-1">
            <span className="h-0.5 w-4 bg-ember-500" />
          </span>
          <div className="min-w-0">
            <p className="truncate text-body font-semibold text-paper-fg">
              A Second, Much Longer Book Title That Wraps
            </p>
            <p className="font-mono text-label text-paper-fg-muted">
              12:41 / 58:20 · Chapter 4
            </p>
          </div>
          <button
            type="button"
            aria-label="Play"
            className="flex h-11 w-11 items-center justify-center rounded-full bg-ember-500 text-ink-950"
          >
            <PlayIcon className="h-5 w-5" />
          </button>
          <div className="hidden flex-1 lg:block">
            <span className="block h-1 rounded-full bg-paper-tone-e">
              <span className="block h-1 w-[22%] rounded-full bg-ember-500" />
            </span>
          </div>
        </div>
        <div className="min-w-0 border-l border-paper-tone-e pl-8">
          <p className="text-label font-semibold tracking-[0.14em] text-paper-fg-muted uppercase">
            Producing
          </p>
          <p className="mt-1 truncate text-body font-semibold text-paper-fg">
            Arabic Grammar Primer
          </p>
          <p className="font-mono text-label text-paper-fg-muted">
            Synthesizing · 43% · ~12 min left
          </p>
        </div>
        <div className="flex items-center gap-3">
          <button
            type="button"
            className="flex items-center gap-2 rounded-xl border border-ember-500/40 px-4 py-2.5 text-body font-semibold text-ember-300"
          >
            <MicIcon className="h-5 w-5" /> Ask the tutor
          </button>
          <span className="flex items-center gap-2 rounded-full bg-paper-tone-e px-3 py-1.5 text-label text-paper-fg-muted">
            <span className="h-2 w-2 rounded-full bg-wash-green-ink" /> Engine
            ready
          </span>
        </div>
      </footer>
    </div>
  );
}

function ConceptSpread() {
  const selected = BOOKS[0];
  return (
    <div className="flex h-screen bg-ink-950 text-ink-100">
      <IconRail />
      <aside className="flex w-[320px] shrink-0 flex-col border-r border-ink-800 bg-ink-900">
        <div className="flex items-center justify-between px-5 pt-6 pb-4">
          <h1 className="font-serif text-title font-semibold text-ink-50">
            Books
          </h1>
          <button
            type="button"
            className="flex items-center gap-1.5 rounded-lg bg-ink-50 px-3 py-1.5 text-secondary font-semibold text-ink-950"
          >
            <UploadIcon className="h-4 w-4" /> Import
          </button>
        </div>
        <div className="flex flex-col gap-1 px-3">
          {BOOKS.map((book, index) => (
            <button
              key={book.title}
              type="button"
              className={`flex gap-3 rounded-xl p-3 text-left ${
                index === 0 ? "bg-ink-800" : "hover:bg-ink-850"
              }`}
            >
              <span
                className={`h-16 w-12 shrink-0 rounded ${book.tone} flex items-end p-1`}
              >
                <span className="h-0.5 w-4 bg-ember-500" />
              </span>
              <span className="min-w-0">
                <span className="block truncate text-body font-semibold text-ink-100">
                  {book.title}
                </span>
                <span className="block truncate text-secondary text-ink-400">
                  {book.author}
                </span>
                <span
                  className={`block truncate text-label ${
                    index === 0 ? "text-ember-400" : "text-ink-500"
                  }`}
                >
                  {book.status}
                </span>
              </span>
            </button>
          ))}
        </div>
      </aside>
      <main className="flex-1 overflow-y-auto px-10 py-8">
        <div className="flex items-start justify-between">
          <div className="flex gap-6">
            <Cover book={selected} className="h-44 w-32" />
            <div>
              <h2 className="font-serif text-display font-semibold text-ink-50">
                {selected.title}
              </h2>
              <p className="mt-1 text-body text-ink-300">{selected.author}</p>
              <p className="mt-2 font-mono text-label text-ink-400">
                {selected.meta} · PDF
              </p>
            </div>
          </div>
          <button
            type="button"
            className="flex items-center gap-2 rounded-xl bg-ink-50 px-4 py-2.5 text-body font-semibold text-ink-950"
          >
            <CastIcon className="h-5 w-5" /> New cast
          </button>
        </div>
        <div className="mt-10 grid grid-cols-3 gap-8">
          <section>
            <h3 className="text-label font-semibold tracking-[0.14em] text-ink-500 uppercase">
              Source
            </h3>
            <div className="mt-3 flex flex-col gap-2 rounded-xl bg-ink-900 p-4">
              <p className="text-body text-ink-200">1. The Alphabet</p>
              <p className="text-body text-ink-200">2. Nouns and Articles</p>
              <p className="text-body text-ink-200">3. Verb Forms</p>
              <p className="text-secondary text-ink-500">…4 more chapters</p>
            </div>
          </section>
          <section>
            <h3 className="text-label font-semibold tracking-[0.14em] text-ink-500 uppercase">
              Casts
            </h3>
            <div className="mt-3 rounded-xl bg-ink-900 p-4">
              <p className="text-body font-semibold text-ink-100">
                Chapters 1–7 · Narrator
              </p>
              <p className="mt-1 text-secondary text-ink-300">
                Synthesizing · 43% · ~12 min left
              </p>
              <span className="mt-3 block h-1 rounded-full bg-ink-800">
                <span className="block h-1 w-[43%] rounded-full bg-ember-500" />
              </span>
            </div>
          </section>
          <section>
            <h3 className="text-label font-semibold tracking-[0.14em] text-ink-500 uppercase">
              Audio
            </h3>
            <div className="mt-3 rounded-xl bg-paper-base-e p-4">
              <p className="text-body font-semibold text-paper-fg">
                Chapters 1–3 · MP3 · 21 MB
              </p>
              <div className="mt-3 flex gap-2">
                <button
                  type="button"
                  className="flex items-center gap-1.5 rounded-lg bg-ember-500 px-3 py-1.5 text-secondary font-semibold text-ink-950"
                >
                  <PlayIcon className="h-4 w-4" /> Play
                </button>
                <button
                  type="button"
                  className="rounded-lg border border-paper-tone-e px-3 py-1.5 text-secondary font-semibold text-paper-fg"
                >
                  Send to phone
                </button>
              </div>
            </div>
          </section>
        </div>
      </main>
    </div>
  );
}

function ReadingTutorSketch() {
  return (
    <div className="flex h-screen bg-ink-950 text-ink-100">
      <aside className="flex w-[360px] shrink-0 flex-col border-r border-ink-800 bg-ink-900 p-5">
        <div className="flex items-center justify-between">
          <p className="flex items-center gap-2 text-body font-semibold text-ink-50">
            <SparkIcon className="h-5 w-5 text-ember-400" /> Voice tutor
          </p>
          <button
            type="button"
            aria-label="Close tutor"
            className="rounded p-1 text-ink-400"
          >
            <CloseIcon className="h-4 w-4" />
          </button>
        </div>
        <div className="mt-2 flex gap-2">
          <span className="rounded-full bg-ink-800 px-2.5 py-1 text-label text-ink-300">
            Live
          </span>
          <span className="rounded-full bg-ink-800 px-2.5 py-1 text-label text-ink-300">
            Audio on
          </span>
          <span className="rounded-full bg-ink-800 px-2.5 py-1 text-label text-ink-300">
            Listening
          </span>
        </div>
        <div className="mt-4 flex flex-1 flex-col gap-3 overflow-hidden">
          <div className="rounded-xl bg-ink-850 p-3 text-secondary text-ink-200">
            Why does the verb come first in this sentence?
          </div>
          <div className="rounded-xl bg-ink-800 p-3 text-secondary text-ink-100">
            In Arabic, verbal sentences lead with the verb. Here the author
            uses…
          </div>
        </div>
        <div className="mt-3 flex items-center gap-2">
          <button
            type="button"
            aria-label="Hold to talk"
            className="flex h-11 w-11 items-center justify-center rounded-full bg-ember-500 text-ink-950"
          >
            <MicIcon className="h-5 w-5" />
          </button>
          <span className="flex-1 rounded-xl bg-ink-850 px-3 py-2.5 text-secondary text-ink-500">
            Type instead…
          </span>
        </div>
      </aside>
      <main className="flex flex-1 gap-8 px-10 py-8">
        <section className="max-w-[640px] flex-1">
          <p className="font-mono text-label text-ink-400">
            Arabic Grammar Primer · Chapter 3
          </p>
          <div className="mt-6 flex flex-col gap-4 text-body leading-relaxed text-ink-200">
            <p>
              The verbal sentence opens with its verb, and the subject follows.
            </p>
            <p className="rounded bg-reading-highlight px-1 text-reading-highlight-fg">
              This ordering is the default in classical prose.
            </p>
            <p>Later chapters return to nominal sentences and their uses.</p>
          </div>
        </section>
        <section className="w-[380px] shrink-0 rounded-xl bg-ink-900 p-4">
          <span className="block aspect-[3/4] rounded bg-paper-base-a" />
        </section>
      </main>
    </div>
  );
}

const CONCEPTS: Record<string, () => React.JSX.Element> = {
  a: ConceptSignalChain,
  b: ConceptControlRoom,
  c: ConceptSpread,
  reading: ReadingTutorSketch,
};

export function v2ConceptFromSearch(
  search: string,
): (() => React.JSX.Element) | null {
  if (!import.meta.env.DEV) return null;
  const params = new URLSearchParams(search);
  const key = params.get("v2concept");
  const concept = key != null ? (CONCEPTS[key] ?? null) : null;
  if (concept != null) {
    document.documentElement.setAttribute(
      "data-theme",
      params.get("theme") === "light" ? "light" : "dark",
    );
  }
  return concept;
}

export default function V2Concepts({
  concept,
}: {
  concept: () => React.JSX.Element;
}) {
  const Concept = concept;
  return <Concept />;
}
