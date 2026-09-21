/**
 * The only module that talks to Tauri.
 *
 * Every `invoke` in the app lives here, behind the `Ipc` interface, so the
 * command-name strings and argument-casing rules exist in exactly one place
 * and the UI can be tested against `mock.ts` with no host.
 */
import { invoke, Channel } from "@tauri-apps/api/core";

import type {
  CardItem,
  Deck,
  DueCard,
  DueCardsArgs,
  Ipc,
  LintDiagnostic,
  LintReport,
  ModelStatus,
  Rating,
  RecentReview,
  ReviewStats,
  VoiceInfo,
} from "./types";

/** What `lint_text` may return: the current shape, or the legacy bare array. */
type RawLintResult =
  | LintDiagnostic[]
  | { diags: LintDiagnostic[]; truncated: boolean };

export const tauriIpc: Ipc = {
  async transcribePcm(pcm, sampleRate, onPartial) {
    // Binary must travel as a Channel-transported Uint8Array; a plain
    // `Vec<u8>` argument would arrive as a number[] and cost ~4x.
    const channel = new Channel<string>();
    channel.onmessage = onPartial;
    return invoke<string>("transcribe_pcm_channel", {
      channel,
      pcmBytes: pcm,
      sampleRate,
    });
  },

  async synthesizeSpeech(text, onChunk) {
    const channel = new Channel<ArrayBuffer>();
    channel.onmessage = onChunk;
    return invoke<number>("synthesize_speech", { text, channel });
  },

  listVoices() {
    return invoke<VoiceInfo[]>("list_voices", {});
  },

  async lintText(text, dialect) {
    // Omit `dialect` entirely when it is the default: the backend treats a
    // missing dialect as American, and sending the string is just noise.
    const args =
      dialect && dialect !== "american" ? { text, dialect } : { text };
    const res = await invoke<RawLintResult>("lint_text", args);
    return normalizeLint(res);
  },

  dueCards(args: DueCardsArgs) {
    // `deckId: undefined` and an absent key are not the same over IPC, so
    // "all decks" omits the key rather than sending an empty string.
    return invoke<DueCard[]>(
      "due_cards",
      args.deckId ? { limit: args.limit, deckId: args.deckId } : { limit: args.limit },
    );
  },

  gradeCard(cardId: string, rating: Rating) {
    return invoke<DueCard | null>("grade_card", { cardId, rating });
  },

  recentReviews(limit: number) {
    return invoke<RecentReview[]>("recent_reviews", { limit });
  },

  reviewStats() {
    return invoke<ReviewStats>("review_stats", {});
  },

  optimizeParameters() {
    return invoke<number[]>("optimize_parameters");
  },

  getRetention() {
    return invoke<number>("get_retention");
  },

  setRetention(retention: number) {
    return invoke<number>("set_retention", { retention });
  },

  listDecks() {
    return invoke<Deck[]>("list_decks", {});
  },

  listCards(deckId?: string) {
    return invoke<CardItem[]>("list_cards", deckId ? { deckId } : {});
  },

  addCard(deckId: string, front: string, back: string) {
    return invoke<string>("add_card", { deckId, front, back });
  },

  deleteCard(cardId: string) {
    return invoke<void>("delete_card", { cardId });
  },

  seedDemoDeck() {
    return invoke<number>("seed_demo_deck");
  },

  modelStatus() {
    return invoke<ModelStatus>("model_status", {});
  },

  epReport() {
    return invoke<string>("ep_report");
  },
};

/**
 * Collapse both lint wire shapes into one.
 *
 * Exported so the normalization is directly testable — it is the kind of
 * compatibility shim that quietly rots otherwise.
 */
export function normalizeLint(res: RawLintResult): LintReport {
  if (Array.isArray(res)) return { diags: res, truncated: false };
  return { diags: res.diags ?? [], truncated: res.truncated === true };
}

let current: Ipc = tauriIpc;

/** The active backend. Use this, never `invoke`. */
export function ipc(): Ipc {
  return current;
}

/** Swap the backend (tests). Returns a function that restores the previous one. */
export function setIpc(next: Ipc): () => void {
  const prev = current;
  current = next;
  return () => {
    current = prev;
  };
}
