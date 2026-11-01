// Drives the installed app over WebView2's debug port: the same commands its UI sends.
import { chromium } from "playwright-core";
import fs from "node:fs";
const [outDir, bookPath, phase = "full"] = process.argv.slice(2);
fs.mkdirSync(outDir, { recursive: true });
const sleep = (ms) => new Promise((r) => setTimeout(r, ms));
const log = (...a) => console.log(new Date().toISOString().slice(11, 19), ...a);
let browser;
for (let i = 0; i < 90 && !browser; i++) {
  try { browser = await chromium.connectOverCDP("http://127.0.0.1:9222"); } catch { await sleep(2000); }
}
if (!browser) throw new Error("no debug port");
let page;
for (let i = 0; i < 60 && !page; i++) {
  page = browser.contexts().flatMap((c) => c.pages()).find((p) => !p.url().startsWith("about:"));
  if (!page) await sleep(1000);
}
const inv = (cmd, args = {}) => page.evaluate(([c, a]) => window.__TAURI_INTERNALS__.invoke(c, a), [cmd, args]);
const shot = (n) => page.screenshot({ path: `${outDir}/${n}.png` }).catch((e) => log("shot failed", n, String(e)));
async function until(what, fn, ms) {
  const end = Date.now() + ms;
  for (;;) {
    const v = await fn().catch((e) => { log(what, "poll error", String(e)); return null; });
    if (v) return v;
    if (Date.now() > end) throw new Error("timeout: " + what);
    await sleep(3000);
  }
}
const ready = () => until("engine ready", async () => (await inv("engine_status")).state === "ready", 15 * 60e3);
await sleep(4000);
await shot(phase + "-start");
log("status", JSON.stringify(await inv("engine_status")));
if (phase === "failure") {
  log("failure", JSON.stringify(await inv("startup_failure")));
  process.exit(0);
}
const info = await inv("engine_install_info");
log("offered", JSON.stringify(info.offered.map((o) => [o.variant, o.downloadBytes])), "recommended", JSON.stringify(info.recommended), "err", info.offerError);
log("keys", JSON.stringify(await inv("voice_key_storage_status").catch(String)));

log("install lite");
await inv("install_engine", { variant: "lite" });
await ready();
log("variant", await inv("engine_active_variant"));
const plan = await inv("plan_import", { path: bookPath });
log("plan", JSON.stringify(plan));
await inv("import_book", { path: bookPath, layout: false });
const books = await inv("list_books");
log("books", books.length);
if (books.length !== 1) throw new Error("import failed");
const bookId = books[0].id;
await shot("after-import");
const local = { bookId, scope: { kind: "pages", page_start: 1, page_end: 1 }, mode: "narrator", outputFormat: "mp3", voiceProfileId: "preset-kokoro-af-heart", modelOverridesJson: "{}" };
const refused = await inv("create_job", { args: local }).then(() => "accepted", String);
log("local cast on reader engine:", refused);
if (!refused.includes("ERR_ENGINE_LOCAL_AI_REQUIRED")) throw new Error("reader engine accepted a local cast");

log("install cpu");
await inv("install_engine", { variant: "cpu" });
await ready();
log("variant", await inv("engine_active_variant"));
log("download kokoro");
await inv("download_model", { modelId: "kokoro-82m" });
await until("kokoro installed", async () => (await inv("list_installed_models")).some((m) => (m.id ?? m.modelId) === "kokoro-82m"), 30 * 60e3);
await until("voice profile", async () => {
  const v = await inv("list_voice_profiles");
  return v.some((p) => p.id === local.voiceProfileId);
}, 120e3).catch(() => log("preset profile absent; relisting", ""));
const jobId = await inv("create_job", { args: local });
log("job", jobId);
const done = await until("job end", async () => {
  const j = await inv("get_job", { jobId });
  log("job state", j.state);
  return ["DONE", "FAILED", "CANCELLED"].includes(j.state) ? j : null;
}, 60 * 60e3);
log("final", JSON.stringify({ state: done.state, error: done.errorMessage ?? done.error, outputs: done.outputPathsJson }));
await shot("after-cast");
fs.writeFileSync(`${outDir}/outputs.json`, done.outputPathsJson ?? "[]");
if (done.state !== "DONE") throw new Error("cast failed");
await browser.close();
