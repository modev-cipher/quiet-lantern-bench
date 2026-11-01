import { describe, expect, it } from "vitest";
import { readSource } from "./tests/readSource";

const SOURCE = readSource(new URL("./Library.tsx", import.meta.url));
const CARD = SOURCE.slice(
  SOURCE.indexOf("function ShelfSlot"),
  SOURCE.indexOf("function ShelfSection"),
);
const MENU = CARD.slice(
  CARD.indexOf("const menuItems"),
  CARD.indexOf("const title = bookDisplayTitle"),
);

// Every book on the shelf renders the same action structure under its
// label, whatever its audio availability: one primary slot, then one
// secondary row in fixed order — Talk, demoted Cast (playable branch
// only), kebab. Delete lives in the kebab menu in every state.
describe("shelf card grammar: one primary slot, one fixed secondary row", () => {
  it("renders exactly one primary slot and one secondary row", () => {
    expect(CARD.match(/data-testid="book-card-primary"/g)).toHaveLength(1);
    expect(CARD.match(/data-testid="book-card-secondary"/g)).toHaveLength(1);
  });

  it("styles both primary branches (Study playable, Cast otherwise) with the one primary class", () => {
    expect(CARD.match(/className=\{SHELF_PRIMARY_CLASS\}/g)).toHaveLength(2);
    const primarySlot = CARD.slice(
      CARD.indexOf('data-testid="book-card-primary"'),
      CARD.indexOf('data-testid="book-card-secondary"'),
    );
    expect(primarySlot).toContain("study-book-");
    expect(primarySlot).toContain("create-audio-");
  });

  it("keeps the secondary row in fixed order: Talk, demoted Cast, kebab", () => {
    const secondary = CARD.slice(
      CARD.indexOf('data-testid="book-card-secondary"'),
    );
    const talk = secondary.indexOf("talk-book-");
    const cast = secondary.indexOf("create-audio-");
    const menu = secondary.indexOf("<BookMenu");
    expect(talk).toBeGreaterThan(-1);
    expect(cast).toBeGreaterThan(talk);
    expect(menu).toBeGreaterThan(cast);
  });

  // F7 P5 (entry gating, Library half): Talk is ungated — any imported book,
  // no playable-export condition around it.
  it("renders Talk unconditionally, outside the playable gate", () => {
    const secondary = CARD.slice(
      CARD.indexOf('data-testid="book-card-secondary"'),
    );
    const talk = secondary.indexOf("talk-book-");
    const gate = secondary.indexOf("availability.playable != null && (");
    expect(talk).toBeGreaterThan(-1);
    expect(gate).toBeGreaterThan(talk);
    expect(secondary).toContain("requestBookTutor(book.id)");
  });

  it("never wraps the action row: the label keeps one fixed height", () => {
    expect(CARD).not.toContain("flex-wrap");
    expect(CARD).not.toContain("flex-col-reverse");
  });

  it("keeps the BookMenu populated in every state so the kebab never disappears, with Delete always inside it", () => {
    expect(MENU).toContain("id: `copy-id-${book.id}`");
    expect(MENU).toContain("testId: `delete-book-${book.id}`");
    const improveGate = MENU.indexOf(
      "canImproveExtraction(book) && !improving",
    );
    const gateEnd = MENU.indexOf(": []),", improveGate);
    expect(gateEnd).toBeGreaterThan(improveGate);
    expect(MENU.indexOf("copy-id-")).toBeGreaterThan(gateEnd);
    expect(MENU.indexOf("delete-book-")).toBeGreaterThan(gateEnd);
    expect(CARD).toContain("testId={book.id}");
    expect(CARD).toContain("items={menuItems}");
  });
});
