import { useEffect, useState } from "react";
import { invoke } from "@tauri-apps/api/core";
import {
  costBasis,
  costLine,
  type CostEstimate,
} from "./castProviderModel";

export function CostFact({
  phase,
  connectionId,
  model,
  bookId,
  scope,
  mode,
}: {
  phase: "script" | "tts";
  connectionId: string;
  model: string | null;
  bookId: string;
  scope: unknown;
  mode: string;
}) {
  const [estimate, setEstimate] = useState<CostEstimate | null>(null);
  useEffect(() => {
    let live = true;
    setEstimate(null);
    invoke<CostEstimate>("estimate_cast_cost", {
      phase,
      connectionId,
      model,
      bookId,
      scope,
      mode,
    })
      .then((value) => live && setEstimate(value))
      .catch(() => live && setEstimate(null));
    return () => {
      live = false;
    };
  }, [phase, connectionId, model, bookId, scope, mode]);
  const basis = costBasis(estimate);
  return (
    <span
      data-testid={`cast-cost-${phase}`}
      className="mt-1 block text-label text-ink-400"
    >
      {costLine(estimate)}
      {basis != null && (
        <span
          data-testid={`cast-cost-basis-${phase}`}
          className="block text-label text-ink-500"
        >
          {basis}
        </span>
      )}
      <span className="block text-label text-ink-500">
        ETA depends on your network and the provider.
      </span>
    </span>
  );
}

