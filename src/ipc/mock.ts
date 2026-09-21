/**
 * An in-memory backend for tests and for developing UI without a Rust build.
 *
 * It is a real implementation of `Ipc`, not a pile of stubs: decks and cards
 * persist across calls within one instance, so a test can add a card and then
 * see it in `listCards`. Anything a test needs to vary is an option.
 */
import type {
  CardItem,
  Deck,
  DueCard,
  Ipc,
  LintReport,
  ModelStatus,
  Rating,
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

export function makeDueCard(over: Partial<DueCard> = {}): DueCard {
  return {
    id: "card-1",
    front: "the front",
    back: "the back",
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
  let retention = options.retention ?? 0.9;
  let nextId = 1;

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
