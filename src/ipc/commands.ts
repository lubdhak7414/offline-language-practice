/**
 * The only module that talks to Tauri.
 *
 * Every `invoke` in the app lives here, behind the `Ipc` interface, so the
 * command-name strings and argument-casing rules exist in exactly one place
 * and the UI can be tested against `mock.ts` with no host.
 */
import { invoke, Channel } from "@tauri-apps/api/core";
import { open, save } from "@tauri-apps/plugin-dialog";

import type {
  AttemptReport,
  AttemptRow,
  BackupInfo,
  BackupResult,
  CardRow,
  DailyLimits,
  DayCount,
  DeckRow,
  DueCard,
  DueCardsArgs,
  ExportDataArgs,
  ExportResult,
  ForecastDay,
  ImportDataArgs,
  ImportSummary,
  Ipc,
  LintDiagnostic,
  LintReport,
  DeckDeleteMode,
  ModelStatus,
  NextPromptArgs,
  Overview,
  Preferences,
  PromptView,
  Rating,
  RetentionBucket,
  ScoreAttemptArgs,
  SetDailyLimitsArgs,
  RecentReview,
  ReviewStats,
  TagRow,
  UndoResult,
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
    return invoke<DueCard[]>("due_cards", {
      limit: args.limit,
      ...(args.deckId ? { deckId: args.deckId } : {}),
      ...(args.tzOffsetMinutes !== undefined
        ? { tzOffsetMinutes: args.tzOffsetMinutes }
        : {}),
    });
  },

  gradeCard(cardId: string, rating: Rating, tzOffsetMinutes?: number) {
    return invoke<DueCard | null>("grade_card", {
      cardId,
      rating,
      ...(tzOffsetMinutes !== undefined ? { tzOffsetMinutes } : {}),
    });
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
    return invoke<DeckRow[]>("list_decks", {});
  },

  listCards(deckId?: string) {
    return invoke<CardRow[]>("list_cards", deckId ? { deckId } : {});
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

  startSession(kind: string) {
    return invoke<string>("start_session", { kind });
  },

  endSession(sessionId: string) {
    return invoke<void>("end_session", { sessionId });
  },

  nextPrompt(args: NextPromptArgs) {
    return invoke<PromptView | null>("next_prompt", {
      sessionId: args.sessionId ?? null,
      category: args.category ?? null,
      level: args.level ?? null,
    });
  },

  seedPrompts() {
    return invoke<number>("seed_prompts");
  },

  scoreAttempt(args: ScoreAttemptArgs) {
    return invoke<AttemptReport>("score_attempt", {
      pcmBytes: args.pcm,
      sampleRate: args.sampleRate,
      sessionId: args.sessionId ?? null,
      promptId: args.promptId ?? null,
      targetText: args.targetText ?? null,
      dialect: args.dialect ?? null,
    });
  },

  listAttempts(sessionId: string | undefined, limit: number) {
    return invoke<AttemptRow[]>("list_attempts", {
      sessionId: sessionId ?? null,
      limit,
    });
  },

  createDeck(name: string) {
    return invoke<string>("create_deck", { name });
  },

  renameDeck(deckId: string, name: string) {
    return invoke<void>("rename_deck", { deckId, name });
  },

  deleteDeck(deckId: string, mode: DeckDeleteMode) {
    return invoke<number>("delete_deck", { deckId, mode });
  },

  updateCard(cardId: string, front: string, back: string) {
    return invoke<void>("update_card", { cardId, front, back });
  },

  modelStatus() {
    return invoke<ModelStatus>("model_status", {});
  },

  epReport() {
    return invoke<string>("ep_report");
  },

  undoReview() {
    return invoke<UndoResult | null>("undo_review", {});
  },

  listTags() {
    return invoke<TagRow[]>("list_tags", {});
  },

  setCardTags(cardId: string, tags: string[]) {
    return invoke<string[]>("set_card_tags", { cardId, tags });
  },

  suspendCard(cardId: string, suspended: boolean) {
    return invoke<void>("suspend_card", { cardId, suspended });
  },

  buryCard(cardId: string, hours?: number) {
    return invoke<number>("bury_card", {
      cardId,
      ...(hours !== undefined ? { hours } : {}),
    });
  },

  getPreferences() {
    return invoke<Preferences>("get_preferences", {});
  },

  setPreferences(prefs: Preferences) {
    return invoke<Preferences>("set_preferences", { prefs });
  },

  getDailyLimits(deckId?: string) {
    return invoke<DailyLimits>("get_daily_limits", deckId ? { deckId } : {});
  },

  setDailyLimits(args: SetDailyLimitsArgs) {
    return invoke<DailyLimits>("set_daily_limits", {
      ...(args.deckId ? { deckId: args.deckId } : {}),
      newPerDay: args.newPerDay,
      reviewPerDay: args.reviewPerDay,
    });
  },

  exportData(args: ExportDataArgs) {
    return invoke<ExportResult>("export_data", {
      ...(args.deckId ? { deckId: args.deckId } : {}),
      path: args.path,
      format: args.format,
    });
  },

  importData(args: ImportDataArgs) {
    return invoke<ImportSummary>("import_data", {
      path: args.path,
      ...(args.deckId ? { deckId: args.deckId } : {}),
    });
  },

  backupDatabase(path: string) {
    return invoke<BackupResult>("backup_database", { path });
  },

  restoreDatabase(path: string) {
    return invoke<BackupInfo>("restore_database", { path });
  },

  statsOverview(tzOffsetMinutes?: number) {
    return invoke<Overview>(
      "stats_overview",
      tzOffsetMinutes !== undefined ? { tzOffsetMinutes } : {},
    );
  },

  statsDaily(days: number, tzOffsetMinutes?: number) {
    return invoke<DayCount[]>("stats_daily", {
      days,
      ...(tzOffsetMinutes !== undefined ? { tzOffsetMinutes } : {}),
    });
  },

  statsForecast(days: number, tzOffsetMinutes?: number) {
    return invoke<ForecastDay[]>("stats_forecast", {
      days,
      ...(tzOffsetMinutes !== undefined ? { tzOffsetMinutes } : {}),
    });
  },

  statsRetention(days: number, bucketDays: number, tzOffsetMinutes?: number) {
    return invoke<RetentionBucket[]>("stats_retention", {
      days,
      bucketDays,
      ...(tzOffsetMinutes !== undefined ? { tzOffsetMinutes } : {}),
    });
  },

  getVoice() {
    return invoke<string>("get_voice", {});
  },

  setVoice(voiceId: string) {
    return invoke<string>("set_voice", { voiceId });
  },

  // Dialogs live inside `ipc/` for the same reason `@tauri-apps/api` does:
  // nothing outside this module may reach for a native picker directly.
  async pickOpenPath(opts: { title: string; extensions: string[] }) {
    const result = await open({
      title: opts.title,
      multiple: false,
      directory: false,
      filters: [{ name: opts.title, extensions: opts.extensions }],
    });
    return typeof result === "string" ? result : null;
  },

  async pickSavePath(opts: { title: string; defaultName: string; extensions: string[] }) {
    const result = await save({
      title: opts.title,
      defaultPath: opts.defaultName,
      filters: [{ name: opts.title, extensions: opts.extensions }],
    });
    return result ?? null;
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
