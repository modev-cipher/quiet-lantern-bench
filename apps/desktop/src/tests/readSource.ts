import { readFileSync } from "node:fs";

export function readSource(path: URL | string): string {
  return readFileSync(path, "utf8").replace(/\r\n/g, "\n");
}
