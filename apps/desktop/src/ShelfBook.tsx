import { convertFileSrc } from "@tauri-apps/api/core";

import TypographicCover from "./TypographicCover";
import {
  bookDisplayAuthor,
  bookDisplayTitle,
  type BookSummary,
} from "./libraryModel";
import {
  bookDirection,
  clothTone,
  ribbonLength,
  spineWidth,
  type ShelfStatus,
} from "./shelfModel";
import { useI18n } from "./i18n";

function spineSurname(author: string | null): string {
  if (author == null) return "";
  const words = author.trim().split(/\s+/);
  return words[words.length - 1] ?? "";
}

function ShelfBook({
  book,
  status,
}: {
  book: BookSummary;
  status: ShelfStatus;
}) {
  const { t } = useI18n();
  const title = bookDisplayTitle(book);
  const author = bookDisplayAuthor(book);
  const seed = book.id || book.title;
  const cloth = `shelf-cloth-${clothTone(seed)}`;
  const thickness = spineWidth(book.pageCount);
  const ribbon = ribbonLength(
    status.kind === "listen" ? status.progress : null,
  );
  const surname = spineSurname(author);

  return (
    <div
      className="shelf-book"
      dir={bookDirection(title)}
      style={{ "--t": `${thickness}px` } as React.CSSProperties}
      data-spine={thickness}
    >
      <div aria-hidden="true" className="shelf-book-head" />
      <div aria-hidden="true" className={`shelf-book-spine ${cloth}`}>
        <span className="shelf-spine-title">{title}</span>
        {thickness >= 16 && <i className="shelf-spine-band" />}
        {thickness >= 18 && surname !== "" && (
          <small className="shelf-spine-author">{surname}</small>
        )}
      </div>
      <div className="shelf-book-front">
        {book.coverPath ? (
          <img
            src={convertFileSrc(book.coverPath)}
            alt={t("library.coverAlt", { title })}
            decoding="async"
            className="h-full w-full object-cover"
          />
        ) : (
          <>
            <TypographicCover
              seed={seed}
              title={title}
              subtitle={author}
              size="shelf"
              toneClassName={cloth}
            />
            <span aria-hidden="true" className="shelf-book-cloth" />
          </>
        )}
        <span aria-hidden="true" className="shelf-book-hinge" />
        <span aria-hidden="true" className="shelf-book-sheen" />
        {status.kind === "casting" && (
          <span
            data-testid={`shelf-casting-${book.id}`}
            aria-hidden="true"
            className="shelf-book-castbar"
          >
            <i />
          </span>
        )}
      </div>
      {ribbon > 0 && (
        <span
          aria-hidden="true"
          data-testid={`shelf-ribbon-${book.id}`}
          className="shelf-book-ribbon"
          style={{ height: ribbon }}
        />
      )}
    </div>
  );
}

export default ShelfBook;
