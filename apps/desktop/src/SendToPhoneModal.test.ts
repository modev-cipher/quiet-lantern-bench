// @vitest-environment jsdom
import { createElement } from "react";
import {
  act,
  cleanup,
  fireEvent,
  render,
  screen,
} from "@testing-library/react";
import { afterEach, beforeEach, expect, it, vi } from "vitest";

const invokeMock = vi.fn<(cmd: string) => Promise<unknown>>();
vi.mock("@tauri-apps/api/core", () => ({
  invoke: (cmd: string) => invokeMock(cmd),
}));
const listenMock =
  vi.fn<
    (
      name: string,
      callback: (event: { payload: LanSendSnapshot }) => void,
    ) => Promise<() => void>
  >();
vi.mock("@tauri-apps/api/event", () => ({
  listen: (
    name: string,
    callback: (event: { payload: LanSendSnapshot }) => void,
  ) => listenMock(name, callback),
}));
import SendToPhoneModal from "./SendToPhoneModal";
import type { LanSendSnapshot } from "./lanSendModel";
import { setLocalePreference } from "./i18n";
import type { AudioShelfItem } from "./audioShelfModel";

const audios: AudioShelfItem[] = [
  {
    id: "export-1",
    jobId: "job-1",
    bookId: "book-1",
    title: "API كتاب 12",
    path: "/audio.mp3",
    bytes: 1000,
    format: "mp3",
    mode: "narrate",
    stale: true,
    isScanned: false,
    createdAt: 1,
  },
];
beforeEach(() => {
  invokeMock.mockReset().mockResolvedValue(null);
  listenMock.mockReset().mockResolvedValue(() => {});
  setLocalePreference("ar");
});
afterEach(() => {
  cleanup();
  vi.useRealTimers();
  invokeMock.mockResolvedValue(null);
  setLocalePreference("en");
});

it("explains selection, stale audio, QR and safety in Arabic with isolated hosts and code", async () => {
  invokeMock.mockImplementation(async (cmd) =>
    cmd === "lan_send_start"
      ? {
          qrSvg: "<svg/>",
          qrPayload: "{}",
          fingerprint: "abcdef0123456789",
          hosts: ["192.168.1.20:47317"],
          port: 47317,
          secretTtlMs: 300000,
        }
      : null,
  );
  render(
    createElement(SendToPhoneModal, { open: true, audios, onClose: vi.fn() }),
  );
  expect(screen.getByRole("dialog", { name: "إرسال إلى الهاتف" })).toBeTruthy();
  expect(screen.getByText("يلزم إعادة الإخراج")).toBeTruthy();
  fireEvent.click(screen.getByTestId("lan-send-show-qr"));
  await screen.findByTestId("lan-send-qr");
  expect(screen.getByText(/امسح هذا الرمز/)).toBeTruthy();
  const code = screen.getByText("abcdef01");
  expect(code.closest("[dir]")?.getAttribute("dir")).toBe("ltr");
  expect(
    screen
      .getByText("192.168.1.20:47317")
      .closest("[dir]")
      ?.getAttribute("dir"),
  ).toBe("ltr");
  expect(screen.getByTestId("lan-send-status").textContent).toBe(
    "في انتظار هاتفك…",
  );
});

it("switches the explanation of an existing failure without re-starting the listener", async () => {
  setLocalePreference("en");
  invokeMock.mockImplementation(async (cmd) => {
    if (cmd === "lan_send_start") throw "no lan address";
    return null;
  });
  render(
    createElement(SendToPhoneModal, { open: true, audios, onClose: vi.fn() }),
  );
  fireEvent.click(screen.getByTestId("lan-send-show-qr"));
  await screen.findByTestId("lan-send-error");
  await act(async () => setLocalePreference("ar"));
  expect(screen.getByTestId("lan-send-error").textContent).toContain(
    "شبكة Wi-Fi نفسها",
  );
  expect(
    invokeMock.mock.calls.filter(([cmd]) => cmd === "lan_send_start"),
  ).toHaveLength(1);
});

it("keeps pairing lock, skipped files, confirmation and completion understandable in Arabic", async () => {
  invokeMock.mockImplementation(async (cmd) =>
    cmd === "lan_send_start"
      ? {
          qrSvg: "<svg/>",
          qrPayload: "{}",
          fingerprint: "abcdef0123456789",
          hosts: ["192.168.1.20:47317"],
          port: 47317,
          secretTtlMs: 300000,
        }
      : null,
  );
  render(
    createElement(SendToPhoneModal, { open: true, audios, onClose: vi.fn() }),
  );
  fireEvent.click(screen.getByTestId("lan-send-show-qr"));
  await screen.findByTestId("lan-send-qr");
  const emit = listenMock.mock.calls[0][1];
  const snapshot: LanSendSnapshot = {
    phase: "sending",
    pairedDevices: 1,
    deviceNames: ["Phone"],
    pairLocked: true,
    acceptedConnections: 1,
    casts: [
      {
        castId: "a",
        title: "كتاب",
        bytesSent: 5,
        bytesTotal: 10,
        missing: false,
        sentComplete: false,
      },
      {
        castId: "b",
        title: "Missing book",
        bytesSent: 0,
        bytesTotal: 0,
        missing: true,
        sentComplete: false,
      },
    ],
  };
  await act(async () => emit({ payload: snapshot }));
  expect(screen.getByTestId("lan-send-pair-locked").textContent).toContain(
    "محاولات إقران فاشلة",
  );
  expect(screen.getByTestId("lan-send-qr-consumed").textContent).toContain(
    "استُخدم هذا الرمز",
  );
  expect(screen.getByTestId("lan-send-progress-b").textContent).toContain(
    "تم تخطّيه",
  );
  fireEvent.click(screen.getByTestId("lan-send-stop"));
  expect(screen.getByTestId("lan-send-stop-confirm").textContent).toContain(
    "سيحتفظ هاتفك بما استلمه",
  );
  fireEvent.click(screen.getByTestId("lan-send-keep"));
  await act(async () =>
    emit({
      payload: {
        ...snapshot,
        phase: "done",
        casts: snapshot.casts.map((cast) => ({
          ...cast,
          sentComplete: !cast.missing,
        })),
      },
    }),
  );
  expect(screen.getByTestId("lan-send-done").textContent).toBe(
    "أُرسل كتاب صوتي واحد إلى هاتفك.",
  );
  fireEvent.click(screen.getByTestId("lan-send-pair-another"));
  await screen.findByTestId("lan-send-qr");
  expect(screen.getByText(/رمز الأمان/).textContent).toContain("abcdef01");
});

it("shows the delayed firewall advice in Arabic without starting another transfer", async () => {
  vi.useFakeTimers();
  invokeMock.mockImplementation(async (cmd) =>
    cmd === "lan_send_start"
      ? {
          qrSvg: "<svg/>",
          qrPayload: "{}",
          fingerprint: "abcdef0123456789",
          hosts: ["192.168.1.20:47317"],
          port: 47317,
          secretTtlMs: 300000,
        }
      : null,
  );
  render(
    createElement(SendToPhoneModal, { open: true, audios, onClose: vi.fn() }),
  );
  await act(async () =>
    fireEvent.click(screen.getByTestId("lan-send-show-qr")),
  );
  await act(async () => vi.advanceTimersByTime(60000));
  expect(screen.getByTestId("lan-send-firewall-hint").textContent).toContain(
    "جدار الحماية",
  );
  expect(
    invokeMock.mock.calls.filter(([cmd]) => cmd === "lan_send_start"),
  ).toHaveLength(1);
});
