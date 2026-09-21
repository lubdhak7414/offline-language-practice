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

export type Deck = { id: string; name: string };

export type CardItem = {
  id: string;
  deck_id: string;
  front: string;
  back: string;
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

export type AttemptReport = {
  attempt_id: string;
  transcript: string;
  target_text: string | null;
  /** `"text"` word alignment, or `"gop"` once acoustic scoring ships. */
  pron_method: string | null;
  /** `null` for free speaking — never a fabricated number. */
  pron_overall: number | null;
  alignment: WordAlignment | null;
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

export type DueCardsArgs = { limit: number; deckId?: string };

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
  gradeCard(cardId: string, rating: Rating): Promise<DueCard | null>;
  recentReviews(limit: number): Promise<RecentReview[]>;
  reviewStats(): Promise<ReviewStats>;
  optimizeParameters(): Promise<number[]>;
  getRetention(): Promise<number>;
  setRetention(retention: number): Promise<number>;

  // --- content ---
  listDecks(): Promise<Deck[]>;
  listCards(deckId?: string): Promise<CardItem[]>;
  addCard(deckId: string, front: string, back: string): Promise<string>;
  deleteCard(cardId: string): Promise<void>;
  seedDemoDeck(): Promise<number>;

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
};
