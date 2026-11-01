import type { DeviceInfo, DeviceInventory } from "./deviceModel";

export function device(
  overrides: Partial<DeviceInfo> = {},
): DeviceInfo {
  return {
    key: "pci:0000:01:00.0",
    backend: "cuda",
    ordinal: 0,
    vendor: "NVIDIA",
    name: "NVIDIA GeForce RTX 3050 Laptop GPU",
    memoryTotalMb: 4096,
    memoryFreeMb: 3767,
    unified: false,
    discrete: true,
    memorySource: "nvml",
    ...overrides,
  };
}

export function inventory(devices: DeviceInfo[]): DeviceInventory {
  return { devices, primaryKey: devices[0]?.key ?? null };
}

export const NO_DEVICES: DeviceInventory = { devices: [], primaryKey: null };
