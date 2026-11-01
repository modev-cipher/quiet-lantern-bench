import { describe, expect, it } from "vitest";

import { NO_DEVICES, device, inventory } from "./deviceFixtures";
import {
  hasGpu,
  primaryDevice,
  primaryVramFreeMb,
  usableMemoryMb,
} from "./deviceModel";

describe("primaryDevice", () => {
  it("looks the planner's choice up rather than ranking devices", () => {
    const igpu = device({
      key: "pci:0000:00:02.0",
      name: "Iris Xe",
      memoryTotalMb: 11_745,
      memoryFreeMb: 6_639,
      unified: true,
      discrete: false,
      backend: "vulkan",
    });
    const dgpu = device({ memoryTotalMb: 4_096, memoryFreeMb: 3_767 });
    const chosen = primaryDevice({
      devices: [igpu, dgpu],
      primaryKey: dgpu.key,
    });
    expect(chosen?.name).toBe("NVIDIA GeForce RTX 3050 Laptop GPU");
    expect(chosen?.memoryFreeMb).toBe(3_767);
  });

  it("returns null when nothing was selected", () => {
    expect(primaryDevice(NO_DEVICES)).toBeNull();
    expect(primaryDevice(null)).toBeNull();
    expect(primaryDevice(undefined)).toBeNull();
  });

  it("returns null when the key names a device that is gone", () => {
    expect(
      primaryDevice({ devices: [device()], primaryKey: "pci:0000:09:00.0" }),
    ).toBeNull();
  });
});

describe("usableMemoryMb", () => {
  it("prefers known free over known total", () => {
    expect(
      usableMemoryMb(device({ memoryTotalMb: 4_096, memoryFreeMb: 3_767 })),
    ).toBe(3_767);
  });

  it("falls back to total when free is unknown", () => {
    expect(
      usableMemoryMb(device({ memoryTotalMb: 4_096, memoryFreeMb: null })),
    ).toBe(4_096);
  });

  it("invents nothing when neither is readable", () => {
    expect(
      usableMemoryMb(device({ memoryTotalMb: null, memoryFreeMb: null })),
    ).toBeNull();
  });
});

describe("hasGpu", () => {
  it("is false on a machine with no devices", () => {
    expect(hasGpu(NO_DEVICES)).toBe(false);
    expect(hasGpu(null)).toBe(false);
  });

  it("is true as soon as any gpu is enumerated", () => {
    expect(hasGpu(inventory([device()]))).toBe(true);
  });

  it("is false for a cpu-only inventory", () => {
    expect(hasGpu(inventory([device({ backend: "cpu" })]))).toBe(false);
  });
});

describe("primaryVramFreeMb", () => {
  it("reports the selected device's free memory", () => {
    expect(
      primaryVramFreeMb(inventory([device({ memoryFreeMb: 3_767 })])),
    ).toBe(3_767);
  });

  it("reports null rather than zero when the driver will not say", () => {
    expect(
      primaryVramFreeMb(
        inventory([device({ memoryFreeMb: null, memoryTotalMb: 4_096 })]),
      ),
    ).toBeNull();
  });
});
