/**
 * Backend → frontend broadcasts.
 *
 * The backend emits exactly one event today (`system-status`); naming it here
 * rather than passing a string at the call site means a rename shows up as a
 * type error instead of a listener that silently never fires.
 */
import { listen, type UnlistenFn } from "@tauri-apps/api/event";

/** Every event the backend emits. */
export type AppEvents = {
  "system-status": string;
};

export type EventName = keyof AppEvents;

export type Events = {
  on<K extends EventName>(
    name: K,
    handler: (payload: AppEvents[K]) => void,
  ): Promise<UnlistenFn>;
};

export const tauriEvents: Events = {
  on(name, handler) {
    return listen(name, (e) => handler(e.payload as never));
  },
};

let current: Events = tauriEvents;

export function events(): Events {
  return current;
}

/** Swap the event source (tests). Returns a function restoring the previous one. */
export function setEvents(next: Events): () => void {
  const prev = current;
  current = next;
  return () => {
    current = prev;
  };
}
