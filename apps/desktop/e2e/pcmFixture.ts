export const FIXTURE_SAMPLE_RATE = 8000;
export const FIXTURE_DURATION_MS = 9000;

function writeAscii(bytes: Uint8Array, offset: number, text: string): void {
  for (let i = 0; i < text.length; i += 1) {
    bytes[offset + i] = text.charCodeAt(i);
  }
}

function writeUint32(bytes: Uint8Array, offset: number, value: number): void {
  bytes[offset] = value & 0xff;
  bytes[offset + 1] = (value >>> 8) & 0xff;
  bytes[offset + 2] = (value >>> 16) & 0xff;
  bytes[offset + 3] = (value >>> 24) & 0xff;
}

function writeUint16(bytes: Uint8Array, offset: number, value: number): void {
  bytes[offset] = value & 0xff;
  bytes[offset + 1] = (value >>> 8) & 0xff;
}

export function buildWav(
  durationMs = FIXTURE_DURATION_MS,
  sampleRate = FIXTURE_SAMPLE_RATE,
): Uint8Array {
  const frames = Math.round((durationMs / 1000) * sampleRate);
  const dataBytes = frames * 2;
  const bytes = new Uint8Array(44 + dataBytes);

  writeAscii(bytes, 0, "RIFF");
  writeUint32(bytes, 4, 36 + dataBytes);
  writeAscii(bytes, 8, "WAVE");
  writeAscii(bytes, 12, "fmt ");
  writeUint32(bytes, 16, 16);
  writeUint16(bytes, 20, 1);
  writeUint16(bytes, 22, 1);
  writeUint32(bytes, 24, sampleRate);
  writeUint32(bytes, 28, sampleRate * 2);
  writeUint16(bytes, 32, 2);
  writeUint16(bytes, 34, 16);
  writeAscii(bytes, 36, "data");
  writeUint32(bytes, 40, dataBytes);

  for (let frame = 0; frame < frames; frame += 1) {
    const seconds = frame / sampleRate;
    const value = Math.round(
      Math.sin(2 * Math.PI * 220 * seconds) * 0.25 * 32767,
    );
    writeUint16(bytes, 44 + frame * 2, value < 0 ? value + 0x10000 : value);
  }
  return bytes;
}

function toBase64(bytes: Uint8Array): string {
  let binary = "";
  const chunk = 0x8000;
  for (let offset = 0; offset < bytes.length; offset += chunk) {
    binary += String.fromCharCode(...bytes.subarray(offset, offset + chunk));
  }
  return typeof btoa === "function"
    ? btoa(binary)
    : Buffer.from(bytes).toString("base64");
}

export function wavDataUrl(durationMs = FIXTURE_DURATION_MS): string {
  return `data:audio/wav;base64,${toBase64(buildWav(durationMs))}`;
}
