import { useCallback, useEffect, useRef, useState } from "preact/hooks";

import { ipc } from "../ipc/commands";
import type { PromptView } from "../ipc/types";
import { MAX_CUSTOM_PROMPT_CHARS } from "../lib/customPrompt";

type Kind = "read" | "free";

/** The backend's message without its "bad input: " prefix. */
function friendly(e: unknown): string {
  return String(e).replace(/^bad input:\s*/i, "");
}

/**
 * Prompts the user writes themselves: a sentence to read aloud (scored) or a
 * question to answer (free speaking, not scored). They are added to whatever
 * category Practice is on, and appear among the random prompts as well.
 */
export function OwnPrompts(props: {
  category: string;
  categoryLabel: string;
  onPractise: (prompt: PromptView) => void;
  announce: (msg: string) => void;
  /** True while a recording or scoring is under way: switching prompts then would lose it. */
  disabled: boolean;
}) {
  const { category, categoryLabel, onPractise, announce, disabled } = props;
  const [kind, setKind] = useState<Kind>("read");
  const [text, setText] = useState("");
  const [level, setLevel] = useState(2);
  const [mine, setMine] = useState<PromptView[]>([]);
  const [error, setError] = useState<string | null>(null);
  const busy = useRef(false);
  const [working, setWorking] = useState(false);

  const refresh = useCallback(async () => {
    try {
      setMine(await ipc().listCustomPrompts());
    } catch (e) {
      setError(friendly(e));
    }
  }, []);

  useEffect(() => {
    void refresh();
  }, [refresh]);

  const add = useCallback(async () => {
    if (busy.current) return;
    busy.current = true;
    setWorking(true);
    setError(null);
    try {
      const added = await ipc().addPrompt({
        category,
        promptText: kind === "free" ? text : "",
        ...(kind === "read" ? { targetText: text } : {}),
        level,
      });
      setText("");
      announce("Prompt added.");
      await refresh();
      onPractise(added);
    } catch (e) {
      setError(friendly(e));
    } finally {
      busy.current = false;
      setWorking(false);
    }
  }, [category, kind, text, level, refresh, onPractise, announce]);

  const remove = useCallback(
    async (id: string) => {
      if (busy.current) return;
      busy.current = true;
      setError(null);
      try {
        await ipc().deletePrompt(id);
        await refresh();
      } catch (e) {
        setError(friendly(e));
      } finally {
        busy.current = false;
      }
    },
    [refresh],
  );

  return (
    <details class="own-prompts">
      <summary>Your own prompts</summary>
      <div class="segmented" role="group" aria-label="Kind of prompt">
        <button type="button" aria-pressed={kind === "read"} onClick={() => setKind("read")}>
          Sentence to read aloud
        </button>
        <button type="button" aria-pressed={kind === "free"} onClick={() => setKind("free")}>
          Question to answer
        </button>
      </div>
      <div class="row">
        <label for="own-prompt-text">{kind === "read" ? "Sentence" : "Question"}</label>
        <textarea
          id="own-prompt-text"
          rows={2}
          maxLength={MAX_CUSTOM_PROMPT_CHARS}
          value={text}
          onInput={(e) => setText((e.target as HTMLTextAreaElement).value)}
          placeholder={
            kind === "read"
              ? "Could you send me the report by Friday?"
              : "Tell me about a time you led a team."
          }
        />
      </div>
      <div class="row">
        <label for="own-prompt-level">Level</label>
        <select
          id="own-prompt-level"
          value={String(level)}
          onChange={(e) => setLevel(Number((e.target as HTMLSelectElement).value))}
        >
          <option value="1">1</option>
          <option value="2">2</option>
          <option value="3">3</option>
        </select>
        <button type="button" disabled={disabled || working || text.trim() === ""} onClick={() => void add()}>
          Add and practise
        </button>
      </div>
      <p class="muted">
        {kind === "read"
          ? "Read-aloud sentences are scored for pronunciation. Use plain letters; write numbers as words. "
          : "Questions are free speaking: grammar and delivery only, no pronunciation score. "}
        Added to {categoryLabel}.
      </p>
      {error && (
        <p class="notice notice-error" role="alert">
          {error}
        </p>
      )}
      {mine.length > 0 && (
        <ul class="own-prompt-list" aria-label="Your prompts">
          {mine.map((p) => (
            <li key={p.id}>
              <span>{p.target_text ?? p.prompt_text}</span>
              <span class="muted">
                {" "}
                {p.category === "interview" ? "interview" : "conversation"}, level {p.level}
              </span>
              <button type="button" disabled={disabled} onClick={() => onPractise(p)}>
                Practise
              </button>
              <button
                type="button"
                aria-label={`Delete: ${p.target_text ?? p.prompt_text}`}
                onClick={() => void remove(p.id)}
              >
                Delete
              </button>
            </li>
          ))}
        </ul>
      )}
    </details>
  );
}
