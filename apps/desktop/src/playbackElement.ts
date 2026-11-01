let element: HTMLAudioElement | null = null;

export function registerAudioElement(audio: HTMLAudioElement | null): void {
  element = audio;
}

export function getAudioElement(): HTMLAudioElement | null {
  return element;
}

export function audioTimeMs(): number {
  if (element == null) return Number.NaN;
  return element.currentTime * 1000;
}
