import type { Page } from "@playwright/test";

export type CastStepId = "scope" | "sound" | "review";

export async function castStep(page: Page, step: CastStepId): Promise<void> {
  await page.getByTestId(`cast-step-${step}`).click();
}
