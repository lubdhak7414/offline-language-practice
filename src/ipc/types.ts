/**
 * Wire types for every backend command, plus the `Ipc` interface the rest of
 * the app talks to.
 *
 * Field names are `snake_case` where the backend serializes a struct, and
 * argument names are `camelCase` because Tauri maps `card_id` <-> `cardId`.
 * That asymmetry is deliberate: it is what actually goes over the wire, and
 * hiding it here would only move the surprise somewhere less obvious.
 */

/** A grade. The backend rejects anything outside 1..=4, so the type says so. */
export type Rating = 1 | 2 | 3 | 4;

export type LintDiagnostic = {
  /** UTF-8 **byte** offsets, not JS char indices. See `lib/text/byteSlice`. */
  start: number;
  end: number;
  message: string;
  suggestions: string[];
  severity?: string;
  rule_id?: string;
};

/** Normalized lint result. The seam hides the bare-array legacy shape. */
export type LintReport = {
  diags: LintDiagnostic[];
  truncated: boolean;
};

/**
 * A deck row. Grew counts in Phase 4 so Decks.tsx can show them without a
 * second round trip per deck.
 */
export type DeckRow = {
  id: string;
  name: string;
  card_count: number;
  due_count: number;
  new_count: number;
};

/**
 * A card row. Grew tags and flags in Phase 4 — see `DeckRow` for the same
 * reasoning.
 */
export type CardRow = {
  id: string;
  deck_id: string;
  front: string;
  back: string;
  tags: string[];
  suspended: boolean;
  buried_until: number;
};

export type ReviewStats = {
  distinct_cards: number;
  total_reviews: number;
  /** Cards with >= 2 reviews; only these can contribute training data. */
  trainable_cards: number;
  /** Prefix items the optimizer will actually train on. */
  train_items: number;
  min_train_items: number;
  min_trainable_cards: number;
};

export type ModelStatus = {
  asr_model: boolean;
  asr_vocab: boolean;
  tts_voice: boolean;
};

export type VoiceInfo = { id: string; label: string };

export type DueCard = {
  id: string;
  front: string;
  back: string;
  deck_id: string;
  deck_name: string | null;
  stability: number;
  difficulty: number;
  days_elapsed: number;
  /** Predicted interval in days, keyed by rating ("1".."4"). */
  intervals: Record<string, number>;
};

export type RecentReview = {
  id: string;
  card_id: string;
  rating: number;
  delta_t: number;
  /** Milliseconds. The backend stores seconds and multiplies on read. */
  reviewed_at: number;
  front?: string;
};

// ─── Practice loop ──────────────────────────────────────────────────────

export type PromptView = {
  id: string;
  category: string;
  topic: string;
  prompt_text: string;
  /** `null` means free speaking: not scored for pronunciation. */
  target_text: string | null;
  level: number;
};

/**
 * One step of the word alignment. Serde tags these with `kind`, so this is a
 * discriminated union and TypeScript narrows on it.
 */
export type WordOp =
  | { kind: "match"; hyp_index: number; target_index: number; word: string }
  | {
      kind: "sub";
      hyp_index: number;
      target_index: number;
      spoken: string;
      expected: string;
    }
  | { kind: "ins"; hyp_index: number; word: string }
  | { kind: "del"; target_index: number; word: string };

export type WordAlignment = {
  ops: WordOp[];
  matched: number;
  substituted: number;
  inserted: number;
  deleted: number;
  /** `null` when there was no reference text to compare against. */
  accuracy: number | null;
};

/** One word's acoustic result. `gop` is always <= 0; 0 is perfect. */
export type WordScore = {
  word: string;
  start_ms: number;
  end_ms: number;
  gop: number;
  score: number;
  verdict: "good" | "unclear" | "poor";
};

export type PronScore = {
  overall: number;
  words: WordScore[];
  target_logprob: number;
  free_logprob: number;
  /** `(0, 1]`, length-independent, so phrases are comparable. */
  normalized_conf: number;
};

export type Pause = { start_ms: number; end_ms: number };

export type FluencyReport = {
  wpm: number;
  /** Words per minute of speaking time, pauses removed. */
  articulation_wpm: number;
  longest_pause_ms: number;
  pause_count: number;
  pauses: Pause[];
  filler_count: number;
  /** `LIKE` alone, which is a filler only about half the time. */
  like_count: number;
  hesitation_count: number;
  speaking_ms: number;
  /** `"aligned"` from word timings, `"energy"` from the audio envelope. */
  method: string;
  score: number;
};

export type AttemptReport = {
  attempt_id: string;
  transcript: string;
  target_text: string | null;
  /** `"gop"` when the acoustic scorer ran, `"text"` when it fell back. */
  pron_method: string | null;
  /** `null` for free speaking — never a fabricated number. */
  pron_overall: number | null;
  alignment: WordAlignment | null;
  /** Per-word acoustic detail; `null` unless `pron_method` is `"gop"`. */
  pron: PronScore | null;
  /** `null` when too little was said to measure delivery honestly. */
  fluency: FluencyReport | null;
  lint: LintReport;
  grammar_score: number;
  duration_ms: number;
  word_count: number;
  overall: number;
  /** Which components went into `overall`, e.g. `["pronunciation","grammar"]`. */
  overall_basis: string[];
};

export type AttemptRow = {
  id: string;
  prompt_id: string | null;
  target_text: string | null;
  transcript: string;
  duration_ms: number;
  /** Milliseconds. */
  created_at: number;
  pron_overall: number | null;
  pron_method: string | null;
  overall: number;
};

export type ScoreAttemptArgs = {
  pcm: Uint8Array;
  sampleRate: number;
  sessionId?: string;
  promptId?: string;
  targetText?: string;
  dialect?: string;
};

export type NextPromptArgs = {
  sessionId?: string;
  category?: string;
  level?: number;
};

/** What happens to a deck's cards when the deck is deleted. */
export type DeckDeleteMode = "move" | "delete";

export type DueCardsArgs = {
  limit: number;
  deckId?: string;
  /** `-new Date().getTimezoneOffset()`. See `lib/tz`. */
  tzOffsetMinutes?: number;
};

// ─── Shared contracts (Phase 4) ─────────────────────────────────────────
//
// These mirror `#[derive(serde::Serialize)]` structs in the Rust backend
// verbatim: plain snake_case fields, no rename attributes. Two backend
// streams (B1, B2) produce these; this file is the seam they must match.

export type Preferences = {
  dialect: string;
  theme: string;
  day_cutoff_hour: number;
  tts_voice: string;
  new_per_day: number;
  review_per_day: number;
  bury_hours: number;
  /** False until first-run onboarding completes. */
  onboarded: boolean;
  /** `everyday` | `interview` | `both`. */
  goal: string;
};

export type DailyLimits = {
  deck_id: string | null;
  new_per_day: number;
  review_per_day: number;
};

export type TagRow = { id: string; name: string; card_count: number };

export type UndoResult = { card_id: string; front: string; rating: number };

export type Overview = {
  total_reviews: number;
  reviews_today: number;
  streak_days: number;
  cards_total: number;
  cards_new: number;
  cards_learning: number;
  cards_mature: number;
  /** `null` on an empty database — never a fabricated 0. */
  retention_30d: number | null;
  practice_ms_30d: number;
  attempts_total: number;
};

/** `day` is the bucket's start, unix **seconds**. */
export type DayCount = { day: number; reviews: number; again: number };

export type ForecastDay = { day: number; due: number };

export type RetentionBucket = { day: number; n: number; ok: number; rate: number };

export type ExportFormat = "json" | "csv" | "tsv";

export type ImportSummary = {
  decks_created: number;
  cards_created: number;
  cards_updated: number;
  reviews_imported: number;
  skipped: number;
  warnings: string[];
};

export type ExportResult = { path: string; cards: number; bytes: number };
export type BackupResult = { path: string; bytes: number };
export type BackupInfo = { cards: number; reviews: number; decks: number };

export type ExportDataArgs = { deckId?: string; path: string; format: ExportFormat };
export type ImportDataArgs = { path: string; deckId?: string };
export type SetDailyLimitsArgs = {
  deckId?: string;
  newPerDay: number;
  reviewPerDay: number;
};

/** A downloadable model group, as onboarding lists it. */
export type ModelGroup = {
  id: string;
  label: string;
  detail: string;
  bytes: number;
  installed: boolean;
};

/**
 * Progress from `download_models`.
 *
 * Mirrors the Rust `DownloadEvent` enum, which serializes with a `kind`
 * tag. Discriminated so a missing case is a type error rather than a
 * silently ignored event.
 */
export type DownloadEvent =
  | {
      kind: "started";
      id: string;
      file: string;
      total: number;
      index: number;
      count: number;
    }
  | { kind: "progress"; id: string; received: number; total: number }
  | { kind: "verifying"; id: string }
  | { kind: "installed"; id: string; file: string }
  | { kind: "done"; installed: string[] }
  | { kind: "failed"; id: string; message: string }
  | { kind: "cancelled"; id: string };

/**
 * Every backend command, in one place.
 *
 * Nothing outside `ipc/` imports `@tauri-apps/api`, so the UI can be rendered
 * and tested without a Tauri host behind it.
 */
export type Ipc = {
  // --- speech ---
  transcribePcm(
    pcm: Uint8Array,
    sampleRate: number,
    onPartial: (text: string) => void,
  ): Promise<string>;
  /** Resolves to the sample rate; WAV chunks arrive via `onChunk`. */
  synthesizeSpeech(
    text: string,
    onChunk: (wav: ArrayBuffer) => void,
  ): Promise<number>;
  listVoices(): Promise<VoiceInfo[]>;

  // --- grammar ---
  lintText(text: string, dialect?: string): Promise<LintReport>;

  // --- review ---
  dueCards(args: DueCardsArgs): Promise<DueCard[]>;
  gradeCard(
    cardId: string,
    rating: Rating,
    tzOffsetMinutes?: number,
  ): Promise<DueCard | null>;
  recentReviews(limit: number): Promise<RecentReview[]>;
  reviewStats(): Promise<ReviewStats>;
  optimizeParameters(): Promise<number[]>;
  getRetention(): Promise<number>;
  setRetention(retention: number): Promise<number>;
  undoReview(): Promise<UndoResult | null>;

  // --- content ---
  listDecks(): Promise<DeckRow[]>;
  listCards(deckId?: string): Promise<CardRow[]>;
  addCard(deckId: string, front: string, back: string): Promise<string>;
  deleteCard(cardId: string): Promise<void>;
  seedDemoDeck(): Promise<number>;
  listTags(): Promise<TagRow[]>;
  setCardTags(cardId: string, tags: string[]): Promise<string[]>;
  suspendCard(cardId: string, suspended: boolean): Promise<void>;
  /** `hours` omitted or 0 clears the bury. Returns the new `buried_until`. */
  buryCard(cardId: string, hours?: number): Promise<number>;

  // --- preferences & caps ---
  getPreferences(): Promise<Preferences>;
  setPreferences(prefs: Preferences): Promise<Preferences>;
  getDailyLimits(deckId?: string): Promise<DailyLimits>;
  setDailyLimits(args: SetDailyLimitsArgs): Promise<DailyLimits>;

  // --- data safety ---
  exportData(args: ExportDataArgs): Promise<ExportResult>;
  importData(args: ImportDataArgs): Promise<ImportSummary>;
  backupDatabase(path: string): Promise<BackupResult>;
  restoreDatabase(path: string): Promise<BackupInfo>;

  // --- stats ---
  statsOverview(tzOffsetMinutes?: number): Promise<Overview>;
  statsDaily(days: number, tzOffsetMinutes?: number): Promise<DayCount[]>;
  statsForecast(days: number, tzOffsetMinutes?: number): Promise<ForecastDay[]>;
  statsRetention(
    days: number,
    bucketDays: number,
    tzOffsetMinutes?: number,
  ): Promise<RetentionBucket[]>;

  // --- voice ---
  getVoice(): Promise<string>;
  setVoice(voiceId: string): Promise<string>;

  // --- dialogs (native file pickers, via @tauri-apps/plugin-dialog) ---
  pickOpenPath(opts: { title: string; extensions: string[] }): Promise<string | null>;
  pickSavePath(opts: {
    title: string;
    defaultName: string;
    extensions: string[];
  }): Promise<string | null>;

  // --- practice loop ---
  startSession(kind: string): Promise<string>;
  endSession(sessionId: string): Promise<void>;
  nextPrompt(args: NextPromptArgs): Promise<PromptView | null>;
  seedPrompts(): Promise<number>;
  scoreAttempt(args: ScoreAttemptArgs): Promise<AttemptReport>;
  listAttempts(sessionId: string | undefined, limit: number): Promise<AttemptRow[]>;

  // --- decks ---
  createDeck(name: string): Promise<string>;
  renameDeck(deckId: string, name: string): Promise<void>;
  /** Returns how many cards were moved or deleted. */
  deleteDeck(deckId: string, mode: DeckDeleteMode): Promise<number>;
  updateCard(cardId: string, front: string, back: string): Promise<void>;

  // --- diagnostics ---
  modelStatus(): Promise<ModelStatus>;
  epReport(): Promise<string>;

  // --- model downloads ---
  listModelCatalog(): Promise<ModelGroup[]>;
  /** Absolute path where in-app downloads install models. */
  modelsDir(): Promise<string>;
  /**
   * Download model groups by id (empty = everything missing). Progress
   * arrives on `onEvent`; resolves with the installed file names.
   */
  downloadModels(
    which: string[],
    onEvent: (ev: DownloadEvent) => void,
  ): Promise<string[]>;
  pauseDownloads(): Promise<void>;
  resumeDownloads(): Promise<void>;
  cancelDownloads(): Promise<void>;
};
