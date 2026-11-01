export type DeviceBackend = "cpu" | "cuda" | "rocm" | "vulkan" | "metal";

export interface DeviceInfo {
  key: string;
  backend: DeviceBackend;
  ordinal: number;
  vendor: string;
  name: string;
  memoryTotalMb: number | null;
  memoryFreeMb: number | null;
  unified: boolean;
  discrete: boolean;
  memorySource: "nvml" | "vk_heap" | "vk_budget" | "rocm" | "unknown";
}

export interface DeviceInventory {
  devices: DeviceInfo[];
  primaryKey: string | null;
}

export function primaryDevice(
  hw: Partial<DeviceInventory> | null | undefined,
): DeviceInfo | null {
  const key = hw?.primaryKey;
  if (!key) return null;
  return (hw?.devices ?? []).find((device) => device.key === key) ?? null;
}

export function primaryVramFreeMb(
  hw: Partial<DeviceInventory> | null | undefined,
): number | null {
  return primaryDevice(hw)?.memoryFreeMb ?? null;
}

export function usableMemoryMb(device: DeviceInfo | null): number | null {
  if (device == null) return null;
  return device.memoryFreeMb ?? device.memoryTotalMb ?? null;
}

export function hasGpu(
  hw: Partial<DeviceInventory> | null | undefined,
): boolean {
  return (hw?.devices ?? []).some((device) => device.backend !== "cpu");
}
