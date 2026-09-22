/**
 * An in-memory backend for tests and for developing UI without a Rust build.
 *
 * It is a real implementation of `Ipc`, not a pile of stubs: decks and cards
 * persist across calls within one instance, so a test can add a card and then
 * see it in `listCards`. Anything a test needs to vary is an option.
 *
 * `mock.ts` must mirror what the backend actually returns — a mock that is a
 * phase behind hides the regression it exists to catch. When the shared
 * contracts in `ipc/types.ts` change, this file changes with them.
 */
import type {
  AttemptReport,
  AttemptRow,
  BackupInfo,
  BackupResult,
  CardRow,
  DailyLimits,
  DayCount,
  DeckDeleteMode,
  DeckRow,
  DueCard,
  ExportDataArgs,
  ExportResult,
  ForecastDay,
  FluencyReport,
  ImportDataArgs,
  ImportSummary,
  Ipc,
  LintReport,
  ModelStatus,
  NextPromptArgs,
  Overview,
  Preferences,
  PromptView,
  PronScore,
  Rating,
  RetentionBucket,
  ScoreAttemptArgs,
  SetDailyLimitsArgs,
  WordAlignment,
  RecentReview,
  ReviewStats,
  TagRow,
  UndoResult,
  VoiceInfo,
} from "./types";

export type MockDeck = { id: string; name: string };
export type MockCard = {
  id: string;
  deck_id: string;
  front: string;
  back: string;
  tags?: string[];
  suspended?: boolean;
  buried_until?: number;
};

export type MockOptions = {
  decks?: MockDeck[];
  cards?: MockCard[];
  due?: DueCard[];
  modelStatus?: ModelStatus;
  stats?: ReviewStats;
  voices?: VoiceInfo[];
  prompts?: PromptView[];
  /** Overrides the report `scoreAttempt` returns. */
  report?: Partial<AttemptReport>;
  lint?: LintReport;
  transcript?: string;
  retention?: number;
  preferences?: Partial<Preferences>;
  overview?: Partial<Overview>;
  daily?: DayCount[];
  forecast?: ForecastDay[];
  retentionBuckets?: RetentionBucket[];
  /** What the next open/save dialog returns; `undefined` means "cancelled". */
  pickOpenPath?: string | null;
  pickSavePath?: string | null;
  /** Command names that should reject, mapped to the error thrown. */
  fail?: Partial<Record<keyof Ipc, unknown>>;
};

export type MockIpc = Ipc & {
  /** Every call made, in order — for asserting what the UI actually asked for. */
  calls: Array<{ name: keyof Ipc; args: unknown[] }>;
};

export function makePrompt(over: Partial<PromptView> = {}): PromptView {
  return {
    id: "builtin-conversation-1",
    category: "conversation",
    topic: "small talk",
    prompt_text: "Reply to a greeting:",
    target_text: "Hi, good to see you again.",
    level: 1,
    ...over,
  };
}

export function makeDueCard(over: Partial<DueCard> = {}): DueCard {
  return {
    id: "card-1",
    front: "the front",
    back: "the back",
    deck_id: "default",
    deck_name: "Default",
    stability: 3.2,
    difficulty: 5.1,
    days_elapsed: 2,
    intervals: { "1": 0.2, "2": 1.4, "3": 4.6, "4": 9.8 },
    ...over,
  };
}

/** Same rule the backend documents: trim, collapse whitespace, lowercase. */
function normalizeTag(raw: string): string | null {
  const collapsed = raw.trim().replace(/\s+/g, "-").toLowerCase();
  return collapsed.length > 0 ? collapsed : null;
}

const DEFAULT_DECKS: MockDeck[] = [
  { id: "default", name: "Default" },
  { id: "travel", name: "Travel" },
  { id: "interview", name: "Interview prep" },
];

const DEFAULT_CARDS: MockCard[] = [
  { id: "card-1", deck_id: "default", front: "How's it going?", back: "Casual greeting.", tags: ["greetings"] },
  {
    id: "card-2",
    deck_id: "travel",
    front: "Could you point me to the station?",
    back: "Asking for directions.",
    tags: ["directions", "travel"],
  },
  {
    id: "card-3",
    deck_id: "travel",
    front: "I'd like a window seat, please.",
    back: "Requesting a preference.",
    tags: ["travel"],
    suspended: true,
  },
  {
    id: "card-4",
    deck_id: "interview",
    front: "Tell me about a time you solved a hard problem.",
    back: "Behavioral prompt: use a concrete example.",
    tags: ["behavioral", "interview"],
  },
  {
    id: "card-5",
    deck_id: "interview",
    front: "What are your salary expectations?",
    back: "Answer with a range, tied to research.",
    tags: ["interview"],
    buried_until: Math.floor(Date.now() / 1000) + 3600,
  },
];

const DEFAULT_PREFERENCES: Preferences = {
  dialect: "american",
  theme: "system",
  day_cutoff_hour: 4,
  tts_voice: "",
  new_per_day: 20,
  review_per_day: 200,
  bury_hours: 20,
};

/** ~30 days of daily counts with a couple of gaps, so charts have to cope. */
function defaultDaily(): DayCount[] {
  const start = Math.floor(Date.now() / 1000 / 86_400) * 86_400 - 29 * 86_400;
  const out: DayCount[] = [];
  for (let i = 0; i < 30; i += 1) {
    const day = start + i * 86_400;
    // Two gap days (nothing reviewed), the rest a plausible small count.
    const isGap = i === 5 || i === 17;
    const reviews = isGap ? 0 : 4 + ((i * 7) % 11);
    const again = isGap ? 0 : Math.floor(reviews / 5);
    out.push({ day, reviews, again });
  }
  return out;
}

function defaultForecast(): ForecastDay[] {
  const start = Math.floor(Date.now() / 1000 / 86_400) * 86_400;
  return Array.from({ length: 14 }, (_, i) => ({
    day: start + i * 86_400,
    due: i === 0 ? 12 : 3 + ((i * 5) % 9),
  }));
}

function defaultRetention(): RetentionBucket[] {
  const start = Math.floor(Date.now() / 1000 / 86_400) * 86_400 - 6 * 7 * 86_400;
  return Array.from({ length: 6 }, (_, i) => {
    const n = 20 + i * 4;
    const ok = Math.round(n * (0.86 + i * 0.01));
    return { day: start + i * 7 * 86_400, n, ok, rate: ok / n };
  });
}

export function createMockIpc(options: MockOptions = {}): MockIpc {
  const decks: MockDeck[] = [...(options.decks ?? DEFAULT_DECKS)];
  const cards: CardRow[] = (options.cards ?? DEFAULT_CARDS).map((c) => ({
    id: c.id,
    deck_id: c.deck_id,
    front: c.front,
    back: c.back,
    tags: [...(c.tags ?? [])],
    suspended: c.suspended ?? false,
    buried_until: c.buried_until ?? 0,
  }));
  const due: DueCard[] = [...(options.due ?? [])];
  const reviews: RecentReview[] = [];
  const attempts: AttemptRow[] = [];
  const prompts: PromptView[] = [...(options.prompts ?? [makePrompt()])];
  let retention = options.retention ?? 0.9;
  let preferences: Preferences = { ...DEFAULT_PREFERENCES, ...options.preferences };
  const dailyLimits = new Map<string, DailyLimits>();
  let voice = preferences.tts_voice;
  let nextId = 1;
  let promptCursor = 0;

  // One undo slot, matching the backend's "newest review only" semantics
  // closely enough for the UI: grading pushes, undo pops.
  type UndoEntry = { review: RecentReview; card: DueCard | undefined; hadCard: boolean };
  const undoStack: UndoEntry[] = [];

  const calls: MockIpc["calls"] = [];
  const record = <T>(name: keyof Ipc, args: unknown[], value: T): Promise<T> => {
    calls.push({ name, args });
    if (options.fail && name in options.fail) {
      return Promise.reject(options.fail[name]);
    }
    return Promise.resolve(value);
  };

  const deckRow = (d: MockDeck): DeckRow => {
    const deckCards = cards.filter((c) => c.deck_id === d.id);
    const dueCount = deckCards.filter(
      (c) => !c.suspended && c.buried_until === 0,
    ).length;
    return {
      id: d.id,
      name: d.name,
      card_count: deckCards.length,
      due_count: Math.min(dueCount, 3),
      new_count: Math.max(0, deckCards.length - dueCount),
    };
  };

  const limitsFor = (deckId: string | null): DailyLimits => {
    const key = deckId ?? "";
    return (
      dailyLimits.get(key) ?? {
        deck_id: deckId,
        new_per_day: preferences.new_per_day,
        review_per_day: preferences.review_per_day,
      }
    );
  };

  return {
    calls,

    transcribePcm(pcm, sampleRate, onPartial) {
      const text = options.transcript ?? "HELLO WORLD";
      onPartial(text);
      return record("transcribePcm", [pcm.byteLength, sampleRate], text);
    },

    synthesizeSpeech(text, onChunk) {
      onChunk(new ArrayBuffer(8));
      return record("synthesizeSpeech", [text], 22050);
    },

    listVoices() {
      return record("listVoices", [], options.voices ?? []);
    },

    lintText(text, dialect) {
      return record(
        "lintText",
        [text, dialect],
        options.lint ?? { diags: [], truncated: false },
      );
    },

    dueCards(args) {
      const pool = args.deckId ? due.filter((c) => c.id.startsWith(args.deckId!)) : due;
      return record("dueCards", [args], pool.slice(0, args.limit));
    },

    gradeCard(cardId: string, rating: Rating, tzOffsetMinutes?: number) {
      const card = due.find((c) => c.id === cardId);
      const review: RecentReview = {
        id: `log-${nextId++}`,
        card_id: cardId,
        rating,
        delta_t: 1,
        reviewed_at: Date.now(),
        ...(card ? { front: card.front } : {}),
      };
      reviews.unshift(review);
      const idx = due.findIndex((c) => c.id === cardId);
      const hadCard = idx >= 0;
      if (hadCard) due.splice(idx, 1);
      undoStack.push({ review, card, hadCard });
      return record("gradeCard", [cardId, rating, tzOffsetMinutes], due[0] ?? null);
    },

    undoReview() {
      const entry = undoStack.pop();
      if (!entry) return record("undoReview", [], null);
      const idx = reviews.findIndex((r) => r.id === entry.review.id);
      if (idx >= 0) reviews.splice(idx, 1);
      if (entry.hadCard && entry.card) due.unshift(entry.card);
      const result: UndoResult = {
        card_id: entry.review.card_id,
        front: entry.review.front ?? "",
        rating: entry.review.rating,
      };
      return record("undoReview", [], result);
    },

    recentReviews(limit: number) {
      return record("recentReviews", [limit], reviews.slice(0, limit));
    },

    reviewStats() {
      return record(
        "reviewStats",
        [],
        options.stats ?? {
          distinct_cards: 0,
          total_reviews: 0,
          trainable_cards: 0,
          train_items: 0,
          min_train_items: 32,
          min_trainable_cards: 8,
        },
      );
    },

    optimizeParameters() {
      return record("optimizeParameters", [], new Array(21).fill(0.5));
    },

    getRetention() {
      return record("getRetention", [], retention);
    },

    setRetention(next: number) {
      retention = next;
      return record("setRetention", [next], next);
    },

    listDecks() {
      return record("listDecks", [], decks.map(deckRow));
    },

    listCards(deckId?: string) {
      const out = deckId ? cards.filter((c) => c.deck_id === deckId) : [...cards];
      return record("listCards", [deckId], out.map((c) => ({ ...c, tags: [...c.tags] })));
    },

    addCard(deckId: string, front: string, back: string) {
      const id = `card-${nextId++}`;
      cards.push({ id, deck_id: deckId, front, back, tags: [], suspended: false, buried_until: 0 });
      return record("addCard", [deckId, front, back], id);
    },

    deleteCard(cardId: string) {
      const idx = cards.findIndex((c) => c.id === cardId);
      if (idx >= 0) cards.splice(idx, 1);
      return record("deleteCard", [cardId], undefined as void);
    },

    seedDemoDeck() {
      return record("seedDemoDeck", [], 0);
    },

    listTags() {
      const counts = new Map<string, number>();
      for (const c of cards) for (const t of c.tags) counts.set(t, (counts.get(t) ?? 0) + 1);
      const rows: TagRow[] = [...counts.entries()]
        .sort(([a], [b]) => a.localeCompare(b))
        .map(([name, card_count]) => ({ id: `tag-${name}`, name, card_count }));
      return record("listTags", [], rows);
    },

    setCardTags(cardId: string, tags: string[]) {
      const normalized = [...new Set(tags.map(normalizeTag).filter((t): t is string => t !== null))];
      const card = cards.find((c) => c.id === cardId);
      if (card) card.tags = normalized;
      return record("setCardTags", [cardId, tags], normalized);
    },

    suspendCard(cardId: string, suspended: boolean) {
      const card = cards.find((c) => c.id === cardId);
      if (card) card.suspended = suspended;
      return record("suspendCard", [cardId, suspended], undefined as void);
    },

    buryCard(cardId: string, hours?: number) {
      const card = cards.find((c) => c.id === cardId);
      const until = hours && hours > 0 ? Math.floor(Date.now() / 1000) + hours * 3600 : 0;
      if (card) card.buried_until = until;
      return record("buryCard", [cardId, hours], until);
    },

    getPreferences() {
      return record("getPreferences", [], { ...preferences });
    },

    setPreferences(prefs: Preferences) {
      preferences = { ...prefs };
      voice = preferences.tts_voice;
      return record("setPreferences", [prefs], { ...preferences });
    },

    getDailyLimits(deckId?: string) {
      return record("getDailyLimits", [deckId], limitsFor(deckId ?? null));
    },

    setDailyLimits(args: SetDailyLimitsArgs) {
      const key = args.deckId ?? "";
      const value: DailyLimits = {
        deck_id: args.deckId ?? null,
        new_per_day: args.newPerDay,
        review_per_day: args.reviewPerDay,
      };
      dailyLimits.set(key, value);
      return record("setDailyLimits", [args], value);
    },

    exportData(args: ExportDataArgs) {
      const count = args.deckId
        ? cards.filter((c) => c.deck_id === args.deckId).length
        : cards.length;
      const result: ExportResult = { path: args.path, cards: count, bytes: count * 128 };
      return record("exportData", [args], result);
    },

    importData(args: ImportDataArgs) {
      const result: ImportSummary = {
        decks_created: 0,
        cards_created: 1,
        cards_updated: 0,
        reviews_imported: 0,
        skipped: 0,
        warnings: [],
      };
      return record("importData", [args], result);
    },

    backupDatabase(path: string) {
      const result: BackupResult = { path, bytes: 4096 };
      return record("backupDatabase", [path], result);
    },

    restoreDatabase(path: string) {
      const result: BackupInfo = { cards: cards.length, reviews: reviews.length, decks: decks.length };
      return record("restoreDatabase", [path], result);
    },

    statsOverview(tzOffsetMinutes?: number) {
      const overview: Overview = {
        total_reviews: 214,
        reviews_today: 12,
        streak_days: 6,
        cards_total: cards.length,
        cards_new: 2,
        cards_learning: 1,
        cards_mature: cards.length - 3,
        retention_30d: 0.91,
        practice_ms_30d: 5_400_000,
        attempts_total: 48,
        ...options.overview,
      };
      return record("statsOverview", [tzOffsetMinutes], overview);
    },

    statsDaily(days: number, tzOffsetMinutes?: number) {
      const rows = options.daily ?? defaultDaily();
      return record("statsDaily", [days, tzOffsetMinutes], rows.slice(-days));
    },

    statsForecast(days: number, tzOffsetMinutes?: number) {
      const rows = options.forecast ?? defaultForecast();
      return record("statsForecast", [days, tzOffsetMinutes], rows.slice(0, days));
    },

    statsRetention(days: number, bucketDays: number, tzOffsetMinutes?: number) {
      const rows = options.retentionBuckets ?? defaultRetention();
      return record("statsRetention", [days, bucketDays, tzOffsetMinutes], rows);
    },

    getVoice() {
      return record("getVoice", [], voice);
    },

    setVoice(voiceId: string) {
      voice = voiceId;
      preferences = { ...preferences, tts_voice: voiceId };
      return record("setVoice", [voiceId], voiceId);
    },

    pickOpenPath(opts: { title: string; extensions: string[] }) {
      return record("pickOpenPath", [opts], options.pickOpenPath ?? null);
    },

    pickSavePath(opts: { title: string; defaultName: string; extensions: string[] }) {
      return record("pickSavePath", [opts], options.pickSavePath ?? null);
    },

    startSession(kind: string) {
      // `calls` already carries the kind, so nothing extra is tracked here.
      return record("startSession", [kind], "session-1");
    },

    endSession(sessionId: string) {
      return record("endSession", [sessionId], undefined as void);
    },

    nextPrompt(args: NextPromptArgs) {
      const pool = args.category
        ? prompts.filter((p) => p.category === args.category)
        : prompts;
      const next = pool[promptCursor % Math.max(1, pool.length)];
      promptCursor += 1;
      return record("nextPrompt", [args], next ?? null);
    },

    seedPrompts() {
      return record("seedPrompts", [], prompts.length);
    },

    scoreAttempt(args: ScoreAttemptArgs) {
      const transcript = options.transcript ?? "HELLO WORLD";
      const scored = args.targetText !== undefined && args.targetText !== "";
      const alignment: WordAlignment | null = scored
        ? {
            ops: [{ kind: "match", hyp_index: 0, target_index: 0, word: "HELLO" }],
            matched: 1,
            substituted: 0,
            inserted: 0,
            deleted: 0,
            accuracy: 100,
          }
        : null;
      // Mirrors the real backend: a read-aloud prompt comes back acoustically
      // scored, free speaking comes back with no pronunciation number at all.
      const pron: PronScore | null = scored
        ? {
            overall: 100,
            words: [
              {
                word: "HELLO",
                start_ms: 0,
                end_ms: 400,
                gop: -0.01,
                score: 100,
                verdict: "good",
              },
            ],
            target_logprob: -2.5,
            free_logprob: -1.5,
            normalized_conf: 0.9,
          }
        : null;
      const fluency: FluencyReport = {
        wpm: 120,
        articulation_wpm: 140,
        longest_pause_ms: 0,
        pause_count: 0,
        pauses: [],
        filler_count: 0,
        like_count: 0,
        hesitation_count: 0,
        speaking_ms: 1800,
        method: scored ? "aligned" : "energy",
        score: 88,
      };
      const report: AttemptReport = {
        attempt_id: `attempt-${nextId++}`,
        transcript,
        target_text: args.targetText ?? null,
        pron_method: scored ? "gop" : null,
        pron_overall: scored ? 100 : null,
        alignment,
        pron,
        fluency,
        lint: options.lint ?? { diags: [], truncated: false },
        grammar_score: 100,
        duration_ms: 2000,
        word_count: 2,
        overall: 96,
        overall_basis: scored
          ? ["pronunciation", "grammar", "fluency"]
          : ["grammar", "fluency"],
        ...options.report,
      };
      attempts.unshift({
        id: report.attempt_id,
        prompt_id: args.promptId ?? null,
        target_text: report.target_text,
        transcript: report.transcript,
        duration_ms: report.duration_ms,
        created_at: Date.now(),
        pron_overall: report.pron_overall,
        pron_method: report.pron_method,
        overall: report.overall,
      });
      return record("scoreAttempt", [args], report);
    },

    listAttempts(sessionId: string | undefined, limit: number) {
      return record("listAttempts", [sessionId, limit], attempts.slice(0, limit));
    },

    createDeck(name: string) {
      const id = `deck-${nextId++}`;
      decks.push({ id, name });
      return record("createDeck", [name], id);
    },

    renameDeck(deckId: string, name: string) {
      const deck = decks.find((d) => d.id === deckId);
      if (deck) deck.name = name;
      return record("renameDeck", [deckId, name], undefined as void);
    },

    deleteDeck(deckId: string, mode: DeckDeleteMode) {
      const affected = cards.filter((c) => c.deck_id === deckId).length;
      if (mode === "move") {
        for (const c of cards) if (c.deck_id === deckId) c.deck_id = "default";
      } else {
        for (let i = cards.length - 1; i >= 0; i -= 1) {
          if (cards[i]?.deck_id === deckId) cards.splice(i, 1);
        }
      }
      const idx = decks.findIndex((d) => d.id === deckId);
      if (idx >= 0) decks.splice(idx, 1);
      return record("deleteDeck", [deckId, mode], affected);
    },

    updateCard(cardId: string, front: string, back: string) {
      const card = cards.find((c) => c.id === cardId);
      if (card) {
        card.front = front;
        card.back = back;
      }
      return record("updateCard", [cardId, front, back], undefined as void);
    },

    modelStatus() {
      return record(
        "modelStatus",
        [],
        options.modelStatus ?? { asr_model: true, asr_vocab: true, tts_voice: true },
      );
    },

    epReport() {
      return record("epReport", [], "inference: cpu");
    },
  };
}
