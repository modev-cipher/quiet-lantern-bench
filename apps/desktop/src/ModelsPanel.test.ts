// @vitest-environment jsdom
import { createElement, StrictMode } from "react";
import {
  act,
  cleanup,
  fireEvent,
  render,
  screen,
  within,
} from "@testing-library/react";
import { afterEach, beforeEach, expect, it, vi } from "vitest";

const invokeMock =
  vi.fn<
    (command: string, args?: Record<string, unknown>) => Promise<unknown>
  >();
vi.mock("@tauri-apps/api/core", () => ({
  invoke: (command: string, args?: Record<string, unknown>) =>
    invokeMock(command, args),
}));
const listenMock =
  vi.fn<
    (
      name: string,
      callback: (event: { payload: unknown }) => void,
    ) => Promise<() => void>
  >();
vi.mock("@tauri-apps/api/event", () => ({
  listen: (name: string, callback: (event: { payload: unknown }) => void) =>
    listenMock(name, callback),
}));
import ModelsPanel from "./ModelsPanel";
import { setLocalePreference } from "./i18n";
import { SETUP_CHANGED_EVENT } from "./views";
import type {
  CatalogModelEntry,
  FullCatalog,
  InstalledModel,
} from "./modelCatalogModel";

const model: CatalogModelEntry = {
  id: "voice-a",
  task: "tts",
  display_name: "Voice A",
  family: "test",
  format: "pytorch",
  disk_bytes: 1000,
  min_free_ram_mb: 1,
  hybrid_capable: false,
  quality_score: 70,
  speed_score: 80,
  license: { spdx: "MIT", commercial_ok: true },
  badge: "green",
  manifest_sha256: "current",
};
const catalog: FullCatalog = {
  catalog_version: "test",
  models: [model, { ...model, id: "voice-b", display_name: "Voice B" }],
};
let installed: InstalledModel[];
let defaults: Record<string, string>;
let failure: string | null;

beforeEach(() => {
  vi.stubGlobal(
    "ResizeObserver",
    class {
      observe = vi.fn();
      disconnect = vi.fn();
    },
  );
  vi.spyOn(console, "error").mockImplementation(() => {});
  installed = catalog.models.map((m) => ({
    modelId: m.id,
    diskBytes: m.disk_bytes,
    sha256: "current",
  }));
  defaults = { tts: "voice-a" };
  failure = null;
  listenMock.mockReset().mockResolvedValue(() => {});
  invokeMock.mockReset().mockImplementation(async (command) => {
    if (command === failure) throw "temporarily unavailable";
    if (command === "get_catalog") return catalog;
    if (command === "list_installed_models") return installed;
    if (command === "get_default_models") return defaults;
    if (command === "get_hardware_profile")
      return { ram: { freeMb: 16000 }, disk: { modelsFreeGb: 20 } };
    return null;
  });
});
afterEach(() => {
  cleanup();
  invokeMock.mockResolvedValue(null);
  setLocalePreference("en");
  vi.restoreAllMocks();
  vi.unstubAllGlobals();
});

it("replaces a failed catalog skeleton with an explanation and working Retry", async () => {
  failure = "get_catalog";
  render(createElement(ModelsPanel));
  const error = await screen.findByRole("alert");
  expect(error.textContent).toContain("Could not load the model catalog");
  failure = null;
  fireEvent.click(within(error).getByRole("button", { name: "Retry" }));
  await screen.findByTestId("installed-models");
  expect(screen.queryByRole("alert")).toBeNull();
});

it("explains a refused removal without suggesting a download retry", async () => {
  render(createElement(ModelsPanel));
  const rows = await screen.findByTestId("installed-models");
  failure = "delete_model";
  fireEvent.click(within(rows).getByTestId("delete-model-voice-a"));
  fireEvent.click(screen.getByTestId("confirm-delete-model-button"));
  const error = await screen.findByRole("alert");
  expect(error.textContent).toContain("Could not finish removing");
  expect(within(rows).getByTestId("model-row-voice-a")).toBeTruthy();
  expect(screen.queryByTestId("download-resume-voice-a")).toBeNull();
});

it("preserves the selected default when the provider-independent local write is refused", async () => {
  render(createElement(ModelsPanel));
  const rows = await screen.findByTestId("installed-models");
  failure = "set_default_model";
  fireEvent.click(within(rows).getByTestId("set-default-voice-b"));
  const error = await screen.findByRole("alert");
  expect(error.textContent).toContain("Could not change the default");
  expect(within(rows).getByTestId("model-default-voice-a")).toBeTruthy();
  expect(within(rows).queryByTestId("model-default-voice-b")).toBeNull();
});

it("does not mistake an empty successful catalog for loading or failure", async () => {
  invokeMock.mockImplementation(async (command) =>
    command === "get_catalog"
      ? { ...catalog, models: [] }
      : command === "get_default_models"
        ? {}
        : command === "list_installed_models"
          ? []
          : null,
  );
  render(createElement(ModelsPanel));
  await screen.findByText("No models are listed in this catalog.");
  expect(screen.queryByRole("alert")).toBeNull();
});

it("admits one model mutation before its acknowledgment", async () => {
  let resolve: () => void = () => {};
  const original = invokeMock.getMockImplementation()!;
  invokeMock.mockImplementation((command, args) =>
    command === "delete_model"
      ? new Promise<void>((done) => {
          resolve = done;
        })
      : original(command, args),
  );
  render(createElement(ModelsPanel));
  const rows = await screen.findByTestId("installed-models");
  fireEvent.click(within(rows).getByTestId("delete-model-voice-a"));
  fireEvent.click(screen.getByTestId("confirm-delete-model-button"));
  fireEvent.click(within(rows).getByTestId("delete-model-voice-b"));
  expect(
    invokeMock.mock.calls.filter(([command]) => command === "delete_model"),
  ).toHaveLength(1);
  await act(async () => resolve());
});

it("retries only the failed refresh after a successful default write", async () => {
  const original = invokeMock.getMockImplementation()!;
  invokeMock.mockImplementation(async (command, args) => {
    if (command === "set_default_model") {
      defaults = { tts: String(args?.modelId) };
      failure = "get_default_models";
      return null;
    }
    return original(command, args);
  });
  render(createElement(ModelsPanel));
  const rows = await screen.findByTestId("installed-models");
  fireEvent.click(within(rows).getByTestId("set-default-voice-b"));
  const error = await screen.findByTestId("models-read-error-defaults");
  expect(screen.queryByTestId("models-action-error")).toBeNull();
  expect(within(rows).getByTestId("model-default-voice-a")).toBeTruthy();
  expect(
    (within(rows).getByTestId("delete-model-voice-a") as HTMLButtonElement)
      .disabled,
  ).toBe(true);
  failure = null;
  const retry = within(error).getByRole("button", { name: "Retry" });
  retry.focus();
  fireEvent.click(retry);
  await within(rows).findByTestId("model-default-voice-b");
  expect(
    invokeMock.mock.calls.filter(
      ([command]) => command === "set_default_model",
    ),
  ).toHaveLength(1);
  expect(document.activeElement).toBe(
    screen.getByRole("heading", { level: 2, name: "Voices & Models" }),
  );
});

it("keeps a partial removal failure reachable after its installed row vanishes", async () => {
  const original = invokeMock.getMockImplementation()!;
  let attempts = 0;
  invokeMock.mockImplementation(async (command, args) => {
    if (command === "delete_model") {
      installed = installed.filter((entry) => entry.modelId !== args?.modelId);
      if (++attempts === 1) throw "default cleanup refused";
      defaults = {};
      return null;
    }
    return original(command, args);
  });
  render(createElement(ModelsPanel));
  const rows = await screen.findByTestId("installed-models");
  const remove = within(rows).getByTestId("delete-model-voice-a");
  remove.focus();
  fireEvent.click(remove);
  fireEvent.click(screen.getByTestId("confirm-delete-model-button"));
  const error = await screen.findByTestId("models-action-error");
  await screen.findByTestId("download-model-voice-a");
  expect(within(rows).queryByTestId("model-row-voice-a")).toBeNull();
  expect(screen.queryByTestId("model-default-voice-a")).toBeNull();
  expect(document.activeElement).toBe(
    screen.getByRole("heading", { level: 2, name: "Voices & Models" }),
  );
  const retry = within(error).getByRole("button", { name: "Retry" });
  await act(async () => fireEvent.click(retry));
  expect(attempts).toBe(2);
  expect(screen.queryByTestId("models-action-error")).toBeNull();
});

it.each(["list_installed_models", "get_default_models"])(
  "shows a recoverable initial %s failure without enabling unknown changes",
  async (command) => {
    failure = command;
    render(createElement(ModelsPanel));
    const error = await screen.findByRole("alert");
    expect(screen.queryByTestId("setup-status")).toBeNull();
    const change =
      screen.queryAllByTestId("delete-model-voice-a")[0] ??
      screen.getByTestId("download-model-voice-a");
    expect((change as HTMLButtonElement).disabled).toBe(true);
    failure = null;
    fireEvent.click(within(error).getByRole("button", { name: "Retry" }));
    await screen.findByTestId("installed-models");
    expect(screen.queryByRole("alert")).toBeNull();
  },
);

it("ignores an older installed snapshot that finishes after a newer refresh", async () => {
  const original = invokeMock.getMockImplementation()!;
  render(createElement(ModelsPanel));
  const rows = await screen.findByTestId("installed-models");
  let old: (value: InstalledModel[]) => void = () => {};
  let latest: (value: InstalledModel[]) => void = () => {};
  let reads = 0;
  invokeMock.mockImplementation((command, args) =>
    command === "list_installed_models"
      ? new Promise<InstalledModel[]>((done) => {
          if (++reads === 1) old = done;
          else latest = done;
        })
      : original(command, args),
  );
  const emit = listenMock.mock.calls.find(
    ([name]) => name === "model-download-finished",
  )![1];
  await act(async () => {
    emit({ payload: { state: "installed", modelId: "voice-a" } });
    emit({ payload: { state: "installed", modelId: "voice-b" } });
  });
  await act(async () =>
    latest(installed.filter((entry) => entry.modelId === "voice-b")),
  );
  expect(within(rows).queryByTestId("model-row-voice-a")).toBeNull();
  await act(async () => old(installed));
  expect(within(rows).queryByTestId("model-row-voice-a")).toBeNull();
});

it("keeps a removal confirmation open while its model status refreshes", async () => {
  const original = invokeMock.getMockImplementation()!;
  render(createElement(ModelsPanel));
  const rows = await screen.findByTestId("installed-models");
  fireEvent.click(within(rows).getByTestId("delete-model-voice-a"));
  let finish: (value: InstalledModel[]) => void = () => {};
  invokeMock.mockImplementation((command, args) =>
    command === "list_installed_models"
      ? new Promise<InstalledModel[]>((done) => {
          finish = done;
        })
      : original(command, args),
  );
  const emit = listenMock.mock.calls.find(
    ([name]) => name === "model-download-finished",
  )![1];
  await act(async () => {
    emit({ payload: { state: "installed", modelId: "voice-b" } });
  });
  const confirm = screen.getByTestId(
    "confirm-delete-model-button",
  ) as HTMLButtonElement;
  expect(confirm.disabled).toBe(true);
  fireEvent.click(confirm);
  expect(screen.getByTestId("delete-model-modal")).toBeTruthy();
  expect(
    invokeMock.mock.calls.filter(([command]) => command === "delete_model"),
  ).toHaveLength(0);
  await act(async () => finish(installed));
  expect(confirm.disabled).toBe(false);
  fireEvent.click(confirm);
  await vi.waitFor(() =>
    expect(
      invokeMock.mock.calls.filter(([command]) => command === "delete_model"),
    ).toHaveLength(1),
  );
});

it("still notifies other screens of a completed write after navigation, without refreshing an unmounted view", async () => {
  const original = invokeMock.getMockImplementation()!;
  let finish: () => void = () => {};
  invokeMock.mockImplementation((command, args) =>
    command === "delete_model"
      ? new Promise<void>((done) => {
          finish = done;
        })
      : original(command, args),
  );
  const listener = vi.fn();
  window.addEventListener(SETUP_CHANGED_EVENT, listener);
  try {
    const view = render(createElement(ModelsPanel));
    const rows = await screen.findByTestId("installed-models");
    fireEvent.click(within(rows).getByTestId("delete-model-voice-a"));
    fireEvent.click(screen.getByTestId("confirm-delete-model-button"));
    view.unmount();
    const calls = invokeMock.mock.calls.length;
    await act(async () => finish());
    expect(listener).toHaveBeenCalledTimes(1);
    expect(invokeMock.mock.calls).toHaveLength(calls);
  } finally {
    window.removeEventListener(SETUP_CHANGED_EVENT, listener);
  }
});

it("translates an open action failure without replaying the write", async () => {
  render(createElement(ModelsPanel));
  const rows = await screen.findByTestId("installed-models");
  failure = "delete_model";
  fireEvent.click(within(rows).getByTestId("delete-model-voice-a"));
  fireEvent.click(screen.getByTestId("confirm-delete-model-button"));
  await screen.findByRole("alert");
  await act(async () => setLocalePreference("ar"));
  expect(screen.getByRole("alert").textContent).toContain("تعذّرت إزالة");
  expect(
    invokeMock.mock.calls.filter(([command]) => command === "delete_model"),
  ).toHaveLength(1);
});

it("allows cancellation while a status read has failed, and keeps the failure specific to cancellation", async () => {
  installed = installed.filter((entry) => entry.modelId === "voice-a");
  failure = "get_default_models";
  render(createElement(ModelsPanel));
  await screen.findByTestId("models-read-error-defaults");
  const emit = listenMock.mock.calls.find(
    ([name]) => name === "model-download-progress",
  )![1];
  await act(async () =>
    emit({
      payload: {
        modelId: "voice-b",
        fileIndex: 0,
        filesTotal: 1,
        bytesDone: 1,
        bytesTotal: 10,
        state: "downloading",
      },
    }),
  );
  failure = "cancel_model_download";
  fireEvent.click(screen.getByTestId("cancel-download-voice-b"));
  const error = await screen.findByTestId("models-action-error");
  expect(error.textContent).toContain("Could not stop the download");
  expect(screen.queryByTestId("download-resume-voice-b")).toBeNull();
  expect(
    invokeMock.mock.calls.filter(
      ([command]) => command === "cancel_model_download",
    ),
  ).toHaveLength(1);
  failure = null;
  await act(async () =>
    fireEvent.click(within(error).getByRole("button", { name: "Retry" })),
  );
  expect(
    invokeMock.mock.calls.filter(
      ([command]) => command === "cancel_model_download",
    ),
  ).toHaveLength(2);
});

it("does not publish a superseded initial error during StrictMode remount", async () => {
  const original = invokeMock.getMockImplementation()!;
  let reads = 0;
  invokeMock.mockImplementation(async (command, args) => {
    if (command === "get_catalog" && ++reads === 1) throw "first read failed";
    return original(command, args);
  });
  render(createElement(StrictMode, null, createElement(ModelsPanel)));
  await screen.findByTestId("installed-models");
  expect(reads).toBe(2);
  expect(screen.queryByRole("alert")).toBeNull();
});
