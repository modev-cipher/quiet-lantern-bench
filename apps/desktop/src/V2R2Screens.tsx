import { AudioIcon, CastIcon, MoreIcon, PlayIcon, UploadIcon } from "./Icons";
import { ORPHAN_EXPORTS, PRIMARY_BOOK, SHELF_BOOKS } from "./V2R2Data";
import { Cover, TopBar } from "./V2R2Shell";

export function HomeScreen() {
  const recent = SHELF_BOOKS.filter(
    (book) => book.id !== PRIMARY_BOOK.id,
  ).slice(0, 4);
  return (
    <div className="flex h-screen flex-col bg-ink-950 text-ink-100">
      <TopBar destination="study" />
      <main className="flex min-h-0 flex-1 justify-center overflow-y-auto">
        <div className="flex w-full max-w-4xl flex-col px-8 pt-[9vh] pb-12">
          <p className="text-label font-semibold tracking-[0.14em] text-ink-500 uppercase">
            Continue studying
          </p>
          <section
            data-testid="v2r2-hero"
            className="mt-4 flex gap-8 rounded-2xl border border-ink-800 bg-ink-900 p-7"
          >
            <Cover book={PRIMARY_BOOK} className="h-52 w-38 shrink-0" />
            <div className="flex min-w-0 flex-col">
              <h1 className="font-serif text-display font-semibold text-ink-50">
                {PRIMARY_BOOK.title}
              </h1>
              <p className="mt-1 text-body text-ink-300">
                {PRIMARY_BOOK.author}
              </p>
              <p className="mt-4 text-body text-ink-200">
                {PRIMARY_BOOK.position}
              </p>
              <p className="mt-1 text-secondary text-ink-400">
                {PRIMARY_BOOK.availability} · {PRIMARY_BOOK.detail}
              </p>
              <div className="mt-auto flex items-center gap-3 pt-5">
                <button
                  type="button"
                  data-testid="v2r2-resume"
                  className="flex items-center gap-2 rounded-xl bg-ember-500 px-5 py-2.5 text-body font-semibold text-ink-950 transition-transform duration-200 active:scale-[0.98]"
                >
                  <PlayIcon className="h-5 w-5" /> Resume
                </button>
                <button
                  type="button"
                  className="rounded-xl border border-ink-800 px-4 py-2.5 text-body font-medium text-ink-200 transition-colors duration-200 hover:text-ink-50 active:scale-[0.98]"
                >
                  Start from the outline
                </button>
              </div>
            </div>
          </section>
          <p className="mt-10 text-label font-semibold tracking-[0.14em] text-ink-500 uppercase">
            Jump back in
          </p>
          <div
            data-testid="v2r2-jump-row"
            className="mt-4 grid grid-cols-2 gap-4 lg:grid-cols-4"
          >
            {recent.map((book) => (
              <button
                key={book.id}
                type="button"
                className="group flex flex-col rounded-xl border border-ink-800 p-3 text-start transition-colors duration-200 hover:bg-ink-900 active:scale-[0.98]"
              >
                <Cover book={book} className="aspect-[4/3]" />
                <span className="mt-3 block truncate text-secondary font-semibold text-ink-100">
                  {book.title}
                </span>
                <span className="mt-0.5 block truncate text-label text-ink-400">
                  {book.position ?? book.availability}
                </span>
              </button>
            ))}
          </div>
        </div>
      </main>
    </div>
  );
}

export function LibraryScreen() {
  return (
    <div className="flex h-screen flex-col bg-ink-950 text-ink-100">
      <TopBar destination="library" />
      <main className="min-h-0 flex-1 overflow-y-auto">
        <div className="mx-auto max-w-shelf px-8 py-8">
          <div className="flex items-end justify-between">
            <div>
              <h1 className="font-serif text-display font-semibold text-ink-50">
                Library
              </h1>
              <p className="mt-1 text-body text-ink-300">
                6 books · 5 with audio
              </p>
            </div>
            <button
              type="button"
              className="flex items-center gap-2 rounded-xl bg-ink-50 px-4 py-2.5 text-body font-semibold text-ink-950 transition-transform duration-200 active:scale-[0.98]"
            >
              <UploadIcon className="h-5 w-5" /> Import book
            </button>
          </div>
          <div
            data-testid="v2r2-library-grid"
            className="mt-8 grid grid-cols-[repeat(auto-fill,minmax(min(180px,100%),1fr))] gap-x-6 gap-y-9"
          >
            {SHELF_BOOKS.map((book) => (
              <article key={book.id} className="flex flex-col">
                <Cover book={book} className="aspect-[3/4]" />
                <p
                  className={`mt-3 truncate text-secondary font-medium ${
                    book.stale ? "text-reading-stale" : "text-ink-200"
                  }`}
                >
                  {book.availability}
                </p>
                <p className="mt-0.5 truncate text-label text-ink-500">
                  {book.detail}
                </p>
                <div className="mt-3 flex items-center gap-2">
                  {book.stale ? (
                    <button
                      type="button"
                      className="flex items-center gap-1.5 rounded-lg bg-ink-800 px-3 py-1.5 text-secondary font-semibold text-ink-100 transition-colors duration-200 hover:bg-ink-700 active:scale-[0.98]"
                    >
                      <CastIcon className="h-4 w-4" /> Re-render
                    </button>
                  ) : book.cast ? (
                    <button
                      type="button"
                      className="flex items-center gap-1.5 rounded-lg bg-ink-800 px-3 py-1.5 text-secondary font-semibold text-ink-100 transition-colors duration-200 hover:bg-ink-700 active:scale-[0.98]"
                    >
                      <PlayIcon className="h-4 w-4" /> Study
                    </button>
                  ) : (
                    <button
                      type="button"
                      className="flex items-center gap-1.5 rounded-lg bg-ink-800 px-3 py-1.5 text-secondary font-semibold text-ink-100 transition-colors duration-200 hover:bg-ink-700 active:scale-[0.98]"
                    >
                      <CastIcon className="h-4 w-4" /> Cast
                    </button>
                  )}
                  <span className="flex-1" />
                  <button
                    type="button"
                    aria-label={`More actions for ${book.title}`}
                    title="Cast more, send to phone, improve extraction, delete"
                    className="rounded-lg border border-ink-800 p-1.5 text-ink-400 transition-colors duration-200 hover:text-ink-100 active:scale-[0.98]"
                  >
                    <MoreIcon className="h-4 w-4" />
                  </button>
                </div>
              </article>
            ))}
          </div>
          <p className="mt-12 text-label font-semibold tracking-[0.14em] text-ink-500 uppercase">
            Unattached audio
          </p>
          <div
            data-testid="v2r2-orphans"
            className="mt-3 flex max-w-panel flex-col gap-2"
          >
            {ORPHAN_EXPORTS.map((orphan) => (
              <div
                key={orphan.title}
                className="flex items-center gap-3 rounded-xl border border-ink-800 px-4 py-3"
              >
                <AudioIcon className="h-5 w-5 shrink-0 text-ink-500" />
                <div className="min-w-0 flex-1">
                  <p className="truncate text-secondary font-medium text-ink-100">
                    {orphan.title}
                  </p>
                  <p
                    className={`truncate text-label ${
                      orphan.missing ? "text-wash-red-ink" : "text-ink-500"
                    }`}
                  >
                    {orphan.meta}
                  </p>
                </div>
                {orphan.missing ? (
                  <button
                    type="button"
                    className="rounded-lg border border-ink-800 px-3 py-1.5 text-secondary font-medium text-ink-400 transition-colors duration-200 hover:text-ink-100 active:scale-[0.98]"
                  >
                    Remove entry
                  </button>
                ) : (
                  <>
                    <button
                      type="button"
                      className="flex items-center gap-1.5 rounded-lg bg-ink-800 px-3 py-1.5 text-secondary font-semibold text-ink-100 transition-colors duration-200 hover:bg-ink-700 active:scale-[0.98]"
                    >
                      <PlayIcon className="h-4 w-4" /> Play
                    </button>
                    <button
                      type="button"
                      className="rounded-lg border border-ink-800 px-3 py-1.5 text-secondary font-medium text-ink-400 transition-colors duration-200 hover:text-ink-100 active:scale-[0.98]"
                    >
                      Send to phone
                    </button>
                    <button
                      type="button"
                      className="rounded-lg border border-ink-800 px-3 py-1.5 text-secondary font-medium text-ink-400 transition-colors duration-200 hover:text-ink-100 active:scale-[0.98]"
                    >
                      Delete
                    </button>
                  </>
                )}
              </div>
            ))}
          </div>
          <p className="mt-2 max-w-panel text-label leading-relaxed text-ink-500">
            Audio you kept after deleting its cast or book lives here — books,
            casts and audio are deleted independently.
          </p>
        </div>
      </main>
    </div>
  );
}
