import { ipc } from "./ipc/commands";
import { events } from "./ipc/events";
import type { DueCard, LintDiagnostic, Rating } from "./ipc/types";
import { concatChunks, resampleTo16k, TARGET_SAMPLE_RATE } from "./lib/audio/resample";
import { friendlyAsrError } from "./lib/errors";
import { formatInterval } from "./lib/interval";
import { goPrefix } from "./lib/globalKeys";
import { reviewKeyAction } from "./lib/keyboard";
import { parseRating } from "./lib/rating";
import { byteSlice } from "./lib/text/byteSlice";

const TRANSCRIPT_PLACEHOLDER = "Press record, speak, then stop.";
const TTS_MAX_CHARS = 1440; // == backend AUDIOSTREAM_MAX_CHARS (8 chunks × 180)
const SYSTEM_TOAST_MS = 4000;
/** Cards fetched per review session. */
const REVIEW_QUEUE_SIZE = 20;

const $ = <T extends HTMLElement>(id: string) => document.getElementById(id) as T;

let mediaStream: MediaStream | null = null;
let audioCtx: AudioContext | null = null;
let processor: ScriptProcessorNode | null = null;
let workletNode: AudioWorkletNode | null = null;
let audioSink: GainNode | null = null;
let pcmChunks: Float32Array[] = [];
let currentCard: DueCard | null = null;
// The whole due queue, not one card at a time: the backend can compute a
// session in one round trip, and "3 of 12" is only possible if we hold it.
let queue: DueCard[] = [];
let sessionTotal = 0;
let revealed = false;
let grading = false;
let transcribing = false;
let hasTranscript = false;
let lastTtsUrl: string | null = null;
let autoStopTimer: ReturnType<typeof setTimeout> | null = null;
let systemToastTimer: ReturnType<typeof setTimeout> | undefined;
// Monotonic request id so a slow loadDue/seed response can't overwrite newer card state.
let cardReq = 0;

// ---- In-flight guard: disable button while async work runs ----
async function withBusy(btn: HTMLButtonElement, fn: () => Promise<void>) {
  btn.disabled = true;
  try {
    await fn();
  } finally {
    btn.disabled = false;
  }
}

function setStatus(id: string, msg: string, isError = false) {
  const el = $(id);
  el.textContent = msg;
  el.classList.toggle("error", isError);
}

// ---- Recording robustness: accept native rate, resample to 16 kHz ----
async function ensure16k(): Promise<AudioContext> {
  // Do NOT hard-throw when the device runs at 44.1/48 kHz; resampling happens later.
  return new AudioContext();
}

function teardownAudio() {
  if (autoStopTimer !== null) {
    clearTimeout(autoStopTimer);
    autoStopTimer = null;
  }
  try {
    processor?.disconnect();
  } catch {
    /* ignore */
  }
  processor = null;
  try {
    workletNode?.disconnect();
  } catch {
    /* ignore */
  }
  workletNode = null;
  try {
    audioSink?.disconnect();
  } catch {
    /* ignore */
  }
  audioSink = null;
  if (mediaStream) {
    mediaStream.getTracks().forEach((t) => t.stop());
    mediaStream = null;
  }
  audioCtx = null;
  pcmChunks = [];
}

// ---- ASR streaming via tauri::ipc::Channel (raw PCM, no JSON base64) ----
async function startRecording() {
  if (transcribing) return;
  if (mediaStream || audioCtx) return;
  try {
    mediaStream = await navigator.mediaDevices.getUserMedia({ audio: { sampleRate: { ideal: 16000 }, channelCount: { ideal: 1 }, echoCancellation: true } });
  } catch (e) {
    setStatus("asr-status", `mic denied/blocked: ${String(e)}`, true);
    throw e;
  }
  let ctx: AudioContext;
  try {
    ctx = await ensure16k();
  } catch (e) {
    mediaStream.getTracks().forEach((t) => t.stop());
    mediaStream = null;
    throw e;
  }
  audioCtx = ctx;
  const src = audioCtx.createMediaStreamSource(mediaStream);
  // zero-gain sink: audible feedback suppressed, keeps the capture node
  // running, no mic echo.
  const sink = audioCtx.createGain();
  sink.gain.value = 0;
  sink.connect(audioCtx.destination);
  audioSink = sink;
  pcmChunks = [];
  // Prefer AudioWorklet over the deprecated ScriptProcessor; fall back for
  // old WebViews that lack AudioWorklet support.
  let workletReady = false;
  try {
    const workletSrc =
      "class Capture extends AudioWorkletProcessor{process(inputs){const ch=inputs[0]&&inputs[0][0];if(ch)this.port.postMessage(ch.slice(0));return true;}}registerProcessor('capture',Capture);";
    const modUrl = URL.createObjectURL(
      new Blob([workletSrc], { type: "application/javascript" })
    );
    try {
      await audioCtx.audioWorklet.addModule(modUrl);
      const node = new AudioWorkletNode(audioCtx, "capture");
      node.port.onmessage = (e: MessageEvent) => {
        pcmChunks.push(new Float32Array(e.data as Float32Array));
      };
      src.connect(node);
      node.connect(sink);
      workletNode = node;
      workletReady = true;
    } finally {
      URL.revokeObjectURL(modUrl);
    }
  } catch {
    workletReady = false;
  }
  if (!workletReady) {
    // 4096-frame chunks at the native rate; resampled to 16 kHz at transcribe time.
    processor = audioCtx.createScriptProcessor(4096, 1, 1);
    processor.onaudioprocess = (e) => {
      pcmChunks.push(new Float32Array(e.inputBuffer.getChannelData(0)));
    };
    src.connect(processor);
    processor.connect(sink);
  }
  ($("btn-record") as HTMLButtonElement).disabled = true;
  ($("btn-stop") as HTMLButtonElement).disabled = false;
  setStatus("asr-status", `recording ${(audioCtx.sampleRate / 1000).toFixed(1)}kHz mono… (resamples to 16k)`);
  // auto-stop at 60s (backend rejects audio > 120s; stay well under)
  autoStopTimer = setTimeout(() => {
    void stopAndTranscribe();
  }, 60000);
}

async function stopAndTranscribe() {
  if (transcribing) return;
  if (!mediaStream && !audioCtx && pcmChunks.length === 0) return;
  // Clear the auto-stop timer first so a manual stop can't double-fire with the timer.
  if (autoStopTimer !== null) {
    clearTimeout(autoStopTimer);
    autoStopTimer = null;
  }
  transcribing = true;
  ($("btn-record") as HTMLButtonElement).disabled = true;
  ($("btn-stop") as HTMLButtonElement).disabled = true;
  const capturedRate = audioCtx?.sampleRate ?? 16000;
  try {
    try {
      processor?.disconnect();
    } catch {
      /* ignore */
    }
    await audioCtx?.close().catch(() => {});
    mediaStream?.getTracks().forEach((t) => t.stop());

    const mono = concatChunks(pcmChunks);

    let pcm16 = mono;
    try {
      pcm16 = await resampleTo16k(mono, capturedRate);
    } catch (e) {
      setStatus("asr-status", `resample failed: ${String(e)}`, true);
      return;
    }
    const usedRate = TARGET_SAMPLE_RATE;

    setStatus("asr-status", `sending ${(pcm16.length / usedRate).toFixed(1)}s PCM via Channel…`);

    try {
      const text = await ipc().transcribePcm(
        new Uint8Array(pcm16.buffer, pcm16.byteOffset, pcm16.byteLength),
        usedRate,
        (partial) => {
          $("transcript").textContent = partial;
        },
      );
      $("transcript").textContent = text;
      ($("tts-input") as HTMLInputElement).value = text;
      hasTranscript = text.trim().length > 0;
      setStatus("asr-status", "done (local Wav2Vec2)");
    } catch (e) {
      setStatus("asr-status", friendlyAsrError(e), true);
    }
  } finally {
    teardownAudio();
    transcribing = false;
    ($("btn-record") as HTMLButtonElement).disabled = false;
    ($("btn-stop") as HTMLButtonElement).disabled = true;
  }
}

// ---- Grammar (harper-core, zero-network) ----
async function lintTranscript() {
  const text = $("transcript").textContent ?? "";
  if (!hasTranscript || !text.trim() || text.trim() === TRANSCRIPT_PLACEHOLDER) {
    setStatus("lint-status", "record something first — nothing to lint", true);
    return;
  }
  setStatus("lint-status", "linting locally…");
  try {
    // Omit `dialect` when it is the default ("american") — the backend
    // treats a missing dialect as the default.
    const dialect = ($("dialect-select") as HTMLSelectElement).value;
    const { diags, truncated } = await ipc().lintText(text, dialect);
    setStatus(
      "lint-status",
      `${diags.length} issue(s)${truncated ? " (truncated)" : ""}`
    );
    renderDiags(text, diags, truncated);
  } catch (e) {
    setStatus("lint-status", `lint failed: ${String(e)}`, true);
  }
}

function renderDiags(text: string, diags: LintDiagnostic[], truncated = false) {
  const box = $("lint-output");
  box.innerHTML = "";
  if (truncated) {
    const note = document.createElement("p");
    note.className = "lint-truncated";
    note.textContent = "Results truncated — showing the first diagnostics only.";
    box.appendChild(note);
  }
  if (diags.length === 0) {
    box.append("Clean — no issues found.");
    return;
  }
  const list = document.createElement("ul");
  for (const d of diags) {
    const li = document.createElement("li");
    const sev = (d.severity ?? "warning").toLowerCase();
    const span = document.createElement("span");
    span.className = `lint-err lint-sev-${sev}`;
    span.textContent = byteSlice(text, d.start, d.end) || "(span)";
    li.append(span, ` [${sev}] — ${d.message}`);
    if (d.rule_id) {
      const rule = document.createElement("code");
      rule.className = "lint-rule";
      rule.textContent = ` (${d.rule_id})`;
      li.appendChild(rule);
    }
    if (d.suggestions.length > 0) {
      li.append(document.createElement("br"), `suggest: ${d.suggestions.slice(0, 3).join(", ")}`);
    }
    list.appendChild(li);
  }
  box.appendChild(list);
}

// ---- TTS (Piper, streamed back via Channel / async URI protocol) ----
async function synthesize() {
  let text = ($("tts-input") as HTMLInputElement).value.trim();
  if (!text) return;
  if (text.length > TTS_MAX_CHARS) {
    text = text.slice(0, TTS_MAX_CHARS);
    ($("tts-input") as HTMLInputElement).value = text;
    setStatus("tts-status", `text over ${TTS_MAX_CHARS} chars — trimmed`, true);
  }
  const audio = $("tts-audio") as HTMLAudioElement;
  // Rust sends one ArrayBuffer per sentence -> accumulate ALL then Blob.
  const chunks: BlobPart[] = [];
  try {
    const sampleRate = await ipc().synthesizeSpeech(text, (buf) =>
      chunks.push(new Uint8Array(buf)),
    );
    // Piper emits WAV; keep WAV MIME and surface the backend-reported rate.
    const blob = new Blob(chunks, { type: "audio/wav" });
    const prevUrl = lastTtsUrl;
    const newUrl = URL.createObjectURL(blob);
    lastTtsUrl = newUrl;
    audio.src = newUrl;
    setStatus("tts-status", `ready ${sampleRate}Hz (${chunks.length} chunk(s))`);
    if (prevUrl) {
      const onCanplay = () => {
        try {
          URL.revokeObjectURL(prevUrl);
        } catch {
          /* ignore */
        }
        audio.removeEventListener("canplay", onCanplay);
      };
      audio.addEventListener("canplay", onCanplay);
    }
    await audio.play().catch((e) => {
      setStatus("tts-status", `playback failed: ${String(e)}`, true);
    });
  } catch (e) {
    const msg = String(e);
    if (msg.includes("TTS_BUSY:")) {
      setStatus("tts-status", "engine busy, retry in a moment", true);
      return;
    }
    // Fallback: async custom protocol (no disk serialization)
    console.warn("channel TTS failed, trying protocol URL", e);
    const prevUrl = lastTtsUrl;
    audio.src = `audiostream://localhost/tts?text=${encodeURIComponent(text)}`;
    if (prevUrl) {
      const onCanplay = () => {
        try {
          URL.revokeObjectURL(prevUrl);
        } catch {
          /* ignore */
        }
        if (lastTtsUrl === prevUrl) lastTtsUrl = null;
        audio.removeEventListener("canplay", onCanplay);
      };
      audio.addEventListener("canplay", onCanplay);
    }
    await audio.play().catch(() => {
      setStatus("tts-status", "TTS playback failed (503 = no voice model?)", true);
    });
  }
}

// ---- FSRS review (2-step: front -> reveal -> grade) ----
/**
 * Write each grade's predicted interval onto its own button.
 *
 * Previously this was one dense line below the row, which meant reading
 * "Again 0.01 / Hard 1.2 / Good 4.6 / Easy 9.8" and mapping it back to the
 * buttons — every single grade.
 */
function renderIntervals(c: DueCard) {
  for (const g of ["1", "2", "3", "4"] as const) {
    const el = document.getElementById(`interval-${g}`);
    if (el) el.textContent = formatInterval(c.intervals[g]);
  }
}

function clearIntervals() {
  for (const g of ["1", "2", "3", "4"] as const) {
    const el = document.getElementById(`interval-${g}`);
    if (el) el.textContent = "";
  }
}

function renderMemoryState(c: DueCard) {
  $("memory-state").textContent =
    `memory (S/D/R) — stability ${c.stability.toFixed(2)} / difficulty ${c.difficulty.toFixed(2)} / ${c.days_elapsed}d elapsed`;
}

function reviewProgress(): string {
  if (sessionTotal === 0) return "0 due";
  const position = sessionTotal - queue.length;
  return `${position} of ${sessionTotal}`;
}

function showFront(c: DueCard) {
  currentCard = c;
  revealed = false;
  $("review-card").textContent = `FRONT: ${c.front}`;
  ($("btn-reveal") as HTMLButtonElement).hidden = false;
  ($("grade-row") as HTMLDivElement).hidden = true;
  renderMemoryState(c);
  renderIntervals(c);
  // A11y: move focus to Reveal so keyboard users can continue without a mouse.
  ($("btn-reveal") as HTMLButtonElement).focus();
}

function reveal() {
  if (!currentCard) return;
  revealed = true;
  $("review-card").textContent = `FRONT: ${currentCard.front}\nBACK: ${currentCard.back}`;
  ($("btn-reveal") as HTMLButtonElement).hidden = true;
  ($("grade-row") as HTMLDivElement).hidden = false;
  document.querySelector<HTMLButtonElement>("#grade-row button")?.focus();
}

function clearCardView(msg: string) {
  currentCard = null;
  queue = [];
  revealed = false;
  $("review-card").textContent = msg;
  ($("btn-reveal") as HTMLButtonElement).hidden = true;
  ($("grade-row") as HTMLDivElement).hidden = true;
  $("memory-state").textContent = "";
  clearIntervals();
}

async function loadDue() {
  const my = ++cardReq;
  try {
    // Deck filter: omit `deckId` (not empty string) when "All decks" is selected.
    const deckSel = document.getElementById("deck-select") as HTMLSelectElement | null;
    const deckId = deckSel?.value || undefined;
    const cards = await ipc().dueCards(
      deckId ? { limit: REVIEW_QUEUE_SIZE, deckId } : { limit: REVIEW_QUEUE_SIZE },
    );
    if (my !== cardReq) return;
    queue = [...cards];
    sessionTotal = cards.length;
    const first = queue.shift();
    if (!first) {
      clearCardView("Nothing due. Seed demo deck or add cards.");
      sessionTotal = 0;
      setStatus("review-status", "0 due");
      return;
    }
    showFront(first);
    setStatus("review-status", reviewProgress());
  } catch (e) {
    if (my !== cardReq) return;
    setStatus("review-status", `review failed: ${String(e)}`, true);
  }
}

async function grade(g: Rating) {
  if (!currentCard || !revealed) return;
  if (grading) return;
  grading = true;
  const my = ++cardReq;
  const gradedId = currentCard.id;
  const btns = Array.from(document.querySelectorAll<HTMLButtonElement>("#grade-row button"));
  btns.forEach((b) => {
    b.disabled = true;
  });
  try {
    const next = await ipc().gradeCard(gradedId, g);
    if (my !== cardReq) return;
    if (currentCard?.id !== gradedId) return;
    // Prefer the local queue so the session length stays stable; the
    // backend's suggestion is the fallback for a queue that ran dry.
    const following = queue.shift() ?? next ?? null;
    if (following) {
      if (following === next) sessionTotal += 1;
      showFront(following);
      setStatus("review-status", reviewProgress());
    } else {
      clearCardView(`Done — ${sessionTotal} card(s) reviewed.`);
      sessionTotal = 0;
      setStatus("review-status", "done");
    }
    void refreshOptimizeGate();
  } catch (e) {
    if (my !== cardReq) return;
    setStatus("review-status", `grade failed: ${String(e)}`, true);
  } finally {
    grading = false;
    btns.forEach((b) => {
      b.disabled = false;
    });
  }
}

async function seed() {
  try {
    const n = await ipc().seedDemoDeck();
    setStatus("review-status", n === 0 ? "already seeded" : `seeded ${n}`);
    await loadDecks();
    await loadDeckCards();
    await loadDue();
  } catch (e) {
    setStatus("review-status", `seed failed: ${String(e)}`, true);
  }
}

// Gate the optimizer button via review_stats{}; the backend owns the minimums.
async function refreshOptimizeGate() {
  const btn = $("btn-optimize") as HTMLButtonElement;
  try {
    const stats = await ipc().reviewStats();
    // Gate on the same two numbers the backend checks. Each card with n
    // reviews contributes n-1 training items, so reviews spread thinly
    // across many cards train less than the raw review count suggests.
    const items = stats.train_items ?? 0;
    const cards = stats.trainable_cards ?? 0;
    const needItems = stats.min_train_items;
    const needCards = stats.min_trainable_cards;
    if (items < needItems || cards < needCards) {
      btn.disabled = true;
      setStatus(
        "optimize-status",
        `need ${items}/${needItems} review histories across ` +
          `${cards}/${needCards} cards before optimizing ` +
          `(each card needs a 2nd review to count)`
      );
    } else {
      btn.disabled = false;
      setStatus(
        "optimize-status",
        `ready (${items} histories across ${cards} cards)`
      );
    }
  } catch (e) {
    // Parallel-wave backend may not have review_stats yet: leave the button
    // enabled and say why there is no pre-check.
    btn.disabled = false;
    setStatus("optimize-status", `optimizer pre-check unavailable: ${String(e)}`, true);
  }
}

async function loadModelStatus() {
  const el = $("model-status");
  // Build the whole line first and write once: an earlier version wrote the
  // failure into `el` from the catch and then unconditionally overwrote it.
  let line: string;
  try {
    const s = await ipc().modelStatus();
    const asr = s.asr_model ? "available" : "missing";
    const vocab = s.asr_vocab ? "available" : "missing";
    const tts = s.tts_voice ? "available" : "missing";
    // No voice <select> in this wave (backend takes no voice param):
    // surface the voice count in the model-status line instead.
    let voices = "unknown";
    try {
      const v = await ipc().listVoices();
      voices = String(v.length);
    } catch {
      voices = "unknown";
    }
    line = `Models — ASR: ${asr} · vocab: ${vocab} · TTS: ${tts} · voices: ${voices}`;
    applyModelGate(s.asr_model && s.asr_vocab, s.tts_voice);
  } catch (e) {
    line = `Models: status unavailable (${String(e)})`;
    // Status unknown: leave the controls enabled rather than locking someone
    // out of a working install because one probe failed.
    applyModelGate(true, true);
  }
  el.textContent = line;
}

/**
 * Enable or disable the controls that need a model on disk.
 *
 * Without this, recording for sixty seconds and *then* being told the model
 * is missing was a perfectly reachable path.
 */
function applyModelGate(asrReady: boolean, ttsReady: boolean) {
  const record = $("btn-record") as HTMLButtonElement;
  record.disabled = !asrReady;
  record.title = asrReady ? "" : "Speech model not installed yet";
  const speak = document.getElementById("btn-speak") as HTMLButtonElement | null;
  if (speak) {
    speak.disabled = !ttsReady;
    speak.title = ttsReady ? "" : "No voice installed yet";
  }
  // Only write on the blocking case: `asr-status` also carries live
  // transcription progress, and clearing it here would stomp on that.
  if (!asrReady) {
    setStatus("asr-status", "Speech model not installed — install it, then reload.", true);
  }
}

async function optimize() {
  setStatus("optimize-status", "optimizing…");
  try {
    const params = await ipc().optimizeParameters();
    setStatus("optimize-status", `optimized: [${params.map((p) => p.toFixed(3)).join(", ")}]`);
  } catch (e) {
    setStatus("optimize-status", `optimize failed: ${String(e)}`, true);
  }
}

async function loadRetention() {
  try {
    const v = await ipc().getRetention();
    ($("retention") as HTMLInputElement).value = String(v);
  } catch (e) {
    setStatus("retention-status", `retention load failed: ${String(e)}`, true);
  }
}

async function saveRetention() {
  const input = $("retention") as HTMLInputElement;
  const raw = input.value.trim();
  const retention = Number(raw);
  if (!raw || Number.isNaN(retention) || retention < 0.7 || retention > 0.98) {
    setStatus("retention-status", "retention must be a number in 0.70–0.98", true);
    return;
  }
  try {
    const updated = await ipc().setRetention(retention);
    input.value = String(updated);
    setStatus("retention-status", `retention ${updated}`);
  } catch (e) {
    setStatus("retention-status", `retention failed: ${String(e)}`, true);
  }
}

async function addCard() {
  const front = ($("card-front") as HTMLInputElement).value.trim();
  const back = ($("card-back") as HTMLInputElement).value.trim();
  if (!front || !back) {
    setStatus("review-status", "front/back required", true);
    return;
  }
  // "All decks" is not a destination. Silently filing the card into
  // "default" meant a card could vanish from the deck the user was looking
  // at, so say so instead.
  const deckId = ($("deck-select") as HTMLSelectElement).value;
  if (!deckId) {
    setStatus("review-status", "choose a deck first — \u201cAll decks\u201d is not a destination", true);
    return;
  }
  try {
    const id = await ipc().addCard(deckId, front, back);
    ($("card-front") as HTMLInputElement).value = "";
    ($("card-back") as HTMLInputElement).value = "";
    setStatus("review-status", `added ${id}`);
    await loadDecks();
    await loadDeckCards();
    await loadDue();
  } catch (e) {
    setStatus("review-status", `add failed: ${String(e)}`, true);
  }
}

async function loadDecks() {
  try {
    const decks = await ipc().listDecks();
    const sel = $("deck-select") as HTMLSelectElement;
    const prev = sel.value;
    sel.innerHTML = "";
    const all = document.createElement("option");
    all.value = "";
    all.textContent = "All decks";
    sel.appendChild(all);
    for (const d of decks) {
      const opt = document.createElement("option");
      opt.value = d.id;
      opt.textContent = d.name;
      sel.appendChild(opt);
    }
    if (prev && Array.from(sel.options).some((o) => o.value === prev)) {
      sel.value = prev;
    }
    setStatus("deck-status", `${decks.length} deck(s)`);
  } catch (e) {
    setStatus("deck-status", `decks unavailable: ${String(e)}`, true);
  }
}

async function loadDeckCards() {
  const box = $("deck-cards");
  try {
    const deckId = ($("deck-select") as HTMLSelectElement).value || undefined;
    const cards = await ipc().listCards(deckId);
    box.innerHTML = "";
    if (cards.length === 0) {
      box.textContent = "No cards in this deck yet.";
      return;
    }
    const list = document.createElement("ul");
    for (const c of cards.slice(0, 50)) {
      const li = document.createElement("li");
      li.textContent = `${c.front} — ${c.back} `;
      const del = document.createElement("button");
      del.textContent = "Delete";
      del.setAttribute("aria-label", `Delete card ${c.front}`);
      del.addEventListener("click", () => void deleteCardById(c.id, c.front));
      li.appendChild(del);
      list.appendChild(li);
    }
    box.appendChild(list);
    if (cards.length > 50) {
      const more = document.createElement("p");
      more.className = "muted";
      more.textContent = `…and ${cards.length - 50} more`;
      box.appendChild(more);
    }
  } catch (e) {
    box.textContent = `cards unavailable: ${String(e)}`;
  }
}

async function deleteCardById(cardId: string, front?: string) {
  if (!window.confirm(`Delete this card${front ? ` "${front}"` : ""}? This cannot be undone.`)) {
    return;
  }
  try {
    await ipc().deleteCard(cardId);
    setStatus("review-status", "card deleted");
    await loadDecks();
    await loadDeckCards();
    await loadDue();
  } catch (e) {
    setStatus("review-status", `delete failed: ${String(e)}`, true);
  }
}

async function deleteCurrentCard() {
  if (!currentCard) {
    setStatus("review-status", "no card loaded", true);
    return;
  }
  await deleteCardById(currentCard.id, currentCard.front);
}

async function loadHistory() {
  try {
    const rows = await ipc().recentReviews(20);
    const box = $("history-output");
    box.innerHTML = "";
    if (rows.length === 0) {
      box.textContent = "No reviews yet.";
      return;
    }
    const table = document.createElement("table");
    const head = document.createElement("tr");
    for (const h of ["card", "rating", "date"]) {
      const th = document.createElement("th");
      th.textContent = h;
      head.appendChild(th);
    }
    table.appendChild(head);
    for (const r of rows) {
      const tr = document.createElement("tr");
      const tdFront = document.createElement("td");
      // `front` snippet replaces the raw card UUID.
      tdFront.textContent = r.front ?? r.card_id;
      const tdRating = document.createElement("td");
      tdRating.textContent = String(r.rating);
      const tdDate = document.createElement("td");
      const ts = r.reviewed_at < 1_000_000_000_000 ? r.reviewed_at * 1000 : r.reviewed_at;
      const parsed = new Date(ts);
      tdDate.textContent = Number.isNaN(parsed.getTime()) ? "—" : parsed.toLocaleString();
      tr.append(tdFront, tdRating, tdDate);
      table.appendChild(tr);
    }
    box.appendChild(table);
  } catch (e) {
    setStatus("review-status", `history failed: ${String(e)}`, true);
  }
}

async function loadEpReport() {
  try {
    const report = await ipc().epReport();
    $("ep-output").textContent = report;
  } catch (e) {
    $("ep-output").textContent = `providers unavailable: ${String(e)}`;
  }
}

function bind() {
  const btnRecord = $("btn-record") as HTMLButtonElement;
  const btnStop = $("btn-stop") as HTMLButtonElement;
  const btnLint = $("btn-lint") as HTMLButtonElement;
  const btnSpeak = $("btn-speak") as HTMLButtonElement;
  const btnDue = $("btn-due") as HTMLButtonElement;
  const btnSeed = $("btn-seed") as HTMLButtonElement;
  const btnOptimize = $("btn-optimize") as HTMLButtonElement;
  const btnRetention = $("btn-retention") as HTMLButtonElement;
  const btnAdd = $("btn-add-card") as HTMLButtonElement;
  const btnDelete = $("btn-delete-card") as HTMLButtonElement;
  const btnHistory = $("btn-history") as HTMLButtonElement;
  const btnEp = $("btn-ep") as HTMLButtonElement;
  const deckSelect = $("deck-select") as HTMLSelectElement;
  const ttsAudio = $("tts-audio") as HTMLAudioElement;

  // TTS object-URL lifetime: revoke only after replacement can play, or when playback ends.
  ttsAudio.addEventListener("canplay", () => {
    // Replacement is playable; per-request cleanup of older URLs happens in synthesize().
  });
  ttsAudio.addEventListener("ended", () => {
    if (lastTtsUrl && ttsAudio.src.startsWith("blob:")) {
      try {
        URL.revokeObjectURL(lastTtsUrl);
      } catch {
        /* ignore */
      }
      lastTtsUrl = null;
    }
  });
  ttsAudio.addEventListener("error", () => {
    const detail = ttsAudio.error?.message ? ` — ${ttsAudio.error.message}` : "";
    setStatus("tts-status", `TTS playback failed (503 = no voice model?)${detail}`, true);
  });

  // Record/stop use withBusy plus end-state fix-up (withBusy re-enables;
  // recording/stopped states must persist after setup/teardown).
  btnRecord.addEventListener("click", () =>
    void (async () => {
      try {
        await withBusy(btnRecord, startRecording);
      } catch {
        /* status already set in startRecording/ensure16k */
      } finally {
        if (transcribing) {
          btnRecord.disabled = true;
          btnStop.disabled = true;
        } else if (mediaStream) {
          btnRecord.disabled = true;
          btnStop.disabled = false;
        }
      }
    })()
  );
  btnStop.addEventListener("click", () =>
    void (async () => {
      try {
        await withBusy(btnStop, stopAndTranscribe);
      } catch {
        /* status already set */
      } finally {
        if (transcribing) {
          btnRecord.disabled = true;
          btnStop.disabled = true;
        } else {
          btnRecord.disabled = false;
          btnStop.disabled = true;
        }
      }
    })()
  );
  btnLint.addEventListener("click", () => void withBusy(btnLint, lintTranscript));
  btnSpeak.addEventListener("click", () => void withBusy(btnSpeak, synthesize));
  btnDue.addEventListener("click", () => void withBusy(btnDue, loadDue));
  btnSeed.addEventListener("click", () => void withBusy(btnSeed, seed));
  // Optimize owns its disabled state (the pre-check gate disables it);
  // withBusy would unconditionally re-enable, so guard + re-gate manually.
  btnOptimize.addEventListener("click", () =>
    void (async () => {
      if (btnOptimize.disabled) return;
      btnOptimize.disabled = true;
      try {
        await optimize();
      } finally {
        await refreshOptimizeGate();
      }
    })()
  );
  btnRetention.addEventListener("click", () => void withBusy(btnRetention, saveRetention));
  btnAdd.addEventListener("click", () => void withBusy(btnAdd, addCard));
  btnDelete.addEventListener("click", () => void withBusy(btnDelete, deleteCurrentCard));
  btnHistory.addEventListener("click", () => void withBusy(btnHistory, loadHistory));
  btnEp.addEventListener("click", () => void withBusy(btnEp, loadEpReport));
  deckSelect.addEventListener("change", () => {
    void loadDeckCards();
    void loadDue();
  });
  $("btn-reveal").addEventListener("click", () => reveal());
  document.querySelectorAll<HTMLButtonElement>("#grade-row button").forEach((b) =>
    b.addEventListener("click", () => {
      const rating = parseRating(b.dataset.grade);
      if (rating) void grade(rating);
    }),
  );
  // Keyboard: 1–4 grades the revealed card; Space/Enter reveals a hidden one.
  // The rule itself lives in `lib/keyboard` so it can be tested directly;
  // this only reads the DOM and dispatches.
  document.addEventListener("keydown", (e) => {
    const t = e.target as HTMLElement | null;
    const action = reviewKeyAction(e.key, {
      targetTag: t?.tagName ?? "",
      isContentEditable: t?.isContentEditable === true,
      isComposing: e.isComposing,
      hasCard: currentCard !== null,
      revealed,
      gradeRowHidden: ($("grade-row") as HTMLDivElement).hidden,
      revealHidden: ($("btn-reveal") as HTMLButtonElement).hidden,
      goPending: goPrefix.armed,
    });
    if (!action) return;
    e.preventDefault();
    if (action.kind === "grade") void grade(action.rating);
    else reveal();
  });
  // Backend → frontend async status broadcasts (Tauri events, not polling).
  // These always go to the toast, never to #asr-status, so they cannot
  // clobber in-progress transcribe state.
  void events().on("system-status", (raw) => {
    const payload = raw ?? "";
    const toast = document.getElementById("system-toast");
    if (!payload || !toast) return;
    toast.textContent = payload;
    window.clearTimeout(systemToastTimer);
    systemToastTimer = window.setTimeout(() => {
      toast.textContent = "";
    }, SYSTEM_TOAST_MS);
  });
}

async function boot() {
  await loadRetention();
  await loadEpReport();
  await loadDecks();
  await loadDeckCards();
  await loadModelStatus();
  await refreshOptimizeGate();
}

/**
 * Wire the harness to the markup currently in the DOM.
 *
 * Called by the Lab route *after* it clones `#lab-template` in. Running at
 * module scope instead would bind before the elements exist, and would leave
 * stale listeners behind if the route were reopened.
 */
export function mountLab() {
  bind();
  void boot();
}
