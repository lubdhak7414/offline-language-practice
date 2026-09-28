/** Mirrors src-tauri/src/practice.rs `validate_new_prompt`, by hand: keep them in step. */
export const MAX_CUSTOM_PROMPT_CHARS = 300;
export const MAX_CUSTOM_PROMPTS = 200;
export const CUSTOM_READ_CUE = "Read this aloud:";
export const CUSTOM_TOPIC = "mine";

export type NewPromptInput = {
  category: string;
  promptText: string;
  targetText?: string;
  level: number;
};

export type ValidPrompt = {
  category: string;
  prompt_text: string;
  target_text: string | null;
  level: number;
};

/** Words the way `pronounce::tokenize` splits them: letters/digits, internal apostrophes kept. */
function words(text: string): string[] {
  const out: string[] = [];
  let word = "";
  const push = () => {
    const trimmed = word.replace(/'+$/, "");
    if (trimmed) out.push(trimmed);
    word = "";
  };
  for (const ch of text) {
    if (/[\p{L}\p{N}]/u.test(ch)) word += ch.toUpperCase();
    else if ((ch === "'" || ch === "’") && word) word += "'";
    else if (word) push();
  }
  push();
  return out;
}

export function validateCustomPrompt(p: NewPromptInput): { ok: true; value: ValidPrompt } | { ok: false; message: string } {
  const bad = (message: string) => ({ ok: false as const, message });
  if (p.category !== "conversation" && p.category !== "interview") {
    return bad("category must be conversation or interview");
  }
  if (![1, 2, 3].includes(p.level)) return bad("level must be 1, 2 or 3");
  const promptText = p.promptText.trim();
  const targetText = (p.targetText ?? "").trim();
  if ([...promptText].length > MAX_CUSTOM_PROMPT_CHARS || [...targetText].length > MAX_CUSTOM_PROMPT_CHARS) {
    return bad(`keep it under ${MAX_CUSTOM_PROMPT_CHARS} characters`);
  }
  if (targetText === "") {
    if (promptText === "") return bad("write a question or a sentence to read");
  } else {
    const tokens = words(targetText);
    if (tokens.length === 0) return bad("the sentence needs at least one word");
    if (tokens.some((w) => !/^[A-Z']+$/.test(w))) {
      return bad("use plain letters and apostrophes only; write numbers and symbols as words");
    }
  }
  return {
    ok: true,
    value: {
      category: p.category,
      prompt_text: promptText === "" ? CUSTOM_READ_CUE : promptText,
      target_text: targetText === "" ? null : targetText,
      level: p.level,
    },
  };
}
