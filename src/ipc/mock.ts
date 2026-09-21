/**
 * An in-memory backend for tests and for developing UI without a Rust build.
 *
 * It is a real implementation of `Ipc`, not a pile of stubs: decks and cards
 * persist across calls within one instance, so a test can add a card and then
 * see it in `listCards`. Anything a test needs to vary is an option.
 */
import type {
  AttemptReport,
  AttemptRow,
  CardItem,
  DeckDeleteMode,
  Deck,
  DueCard,
  Ipc,
  LintReport,
  ModelStatus,
  NextPromptArgs,
  PromptView,
  Rating,
  ScoreAttemptArgs,
  WordAlignment,
  RecentReview,
  ReviewStats,
  VoiceInfo,
} from "./types";

export type MockOptions = {
  decks?: Deck[];
  cards?: CardItem[];
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

export function createMockIpc(options: MockOptions = {}): MockIpc {
  const decks: Deck[] = [...(options.decks ?? [{ id: "default", name: "Default" }])];
  const cards: CardItem[] = [...(options.cards ?? [])];
  const due: DueCard[] = [...(options.due ?? [])];
  const reviews: RecentReview[] = [];
  const attempts: AttemptRow[] = [];
  const prompts: PromptView[] = [...(options.prompts ?? [makePrompt()])];
  let retention = options.retention ?? 0.9;
  let nextId = 1;
  let promptCursor = 0;

  const calls: MockIpc["calls"] = [];
  const record = <T>(name: keyof Ipc, args: unknown[], value: T): Promise<T> => {
    calls.push({ name, args });
    if (options.fail && name in options.fail) {
      return Promise.reject(options.fail[name]);
    }
    return Promise.resolve(value);
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

    gradeCard(cardId: string, rating: Rating) {
      reviews.unshift({
        id: `log-${nextId++}`,
        card_id: cardId,
        rating,
        delta_t: 1,
        reviewed_at: Date.now(),
      });
      const idx = due.findIndex((c) => c.id === cardId);
      if (idx >= 0) due.splice(idx, 1);
      return record("gradeCard", [cardId, rating], due[0] ?? null);
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
      return record("listDecks", [], [...decks]);
    },

    listCards(deckId?: string) {
      const out = deckId ? cards.filter((c) => c.deck_id === deckId) : [...cards];
      return record("listCards", [deckId], out);
    },

    addCard(deckId: string, front: string, back: string) {
      const id = `card-${nextId++}`;
      cards.push({ id, deck_id: deckId, front, back });
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
      const report: AttemptReport = {
        attempt_id: `attempt-${nextId++}`,
        transcript,
        target_text: args.targetText ?? null,
        pron_method: scored ? "text" : null,
        pron_overall: scored ? 100 : null,
        alignment,
        lint: options.lint ?? { diags: [], truncated: false },
        grammar_score: 100,
        duration_ms: 2000,
        word_count: 2,
        overall: 100,
        overall_basis: scored ? ["pronunciation", "grammar"] : ["grammar"],
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
