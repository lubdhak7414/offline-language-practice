import { useCallback, useEffect, useRef, useState } from "preact/hooks";

import { ipc } from "../ipc/commands";
import type { CardRow, DeckDeleteMode, DeckRow } from "../ipc/types";

/** The two things a bury button offers: bury for a day, or clear it. */
const BURY_HOURS = 20;

export function Decks(props: { announce: (msg: string) => void }) {
  const { announce } = props;
  const [decks, setDecks] = useState<DeckRow[]>([]);
  const [selected, setSelected] = useState<string | null>(null);
  const [cards, setCards] = useState<CardRow[]>([]);
  const [loading, setLoading] = useState(true);
  const [error, setError] = useState<string | null>(null);

  const [newDeckName, setNewDeckName] = useState("");
  const [renaming, setRenaming] = useState<{ id: string; name: string } | null>(null);
  const [deleteTarget, setDeleteTarget] = useState<DeckRow | null>(null);
  const [deleteMode, setDeleteMode] = useState<DeckDeleteMode>("move");

  const [newFront, setNewFront] = useState("");
  const [newBack, setNewBack] = useState("");
  const [editing, setEditing] = useState<{ id: string; front: string; back: string } | null>(null);
  const [tagDrafts, setTagDrafts] = useState<Record<string, string>>({});
  const [checked, setChecked] = useState<Set<string>>(new Set());

  // Guards a ref, not state: two clicks in the same tick both read the state
  // from their own closure, so a destructive action needs the real guard to
  // live outside render. See the note in Review.tsx for the same pattern.
  const busy = useRef(false);

  const loadDecks = useCallback(async () => {
    try {
      const rows = await ipc().listDecks();
      setDecks(rows);
    } catch (e) {
      setError(String(e));
    }
  }, []);

  const loadCards = useCallback(async (deckId: string | null) => {
    setLoading(true);
    setError(null);
    try {
      const rows = await ipc().listCards(deckId ?? undefined);
      setCards(rows);
      setChecked(new Set());
    } catch (e) {
      setError(String(e));
    } finally {
      setLoading(false);
    }
  }, []);

  useEffect(() => {
    void loadDecks();
  }, [loadDecks]);

  useEffect(() => {
    void loadCards(selected);
  }, [selected, loadCards]);

  const createDeck = useCallback(async () => {
    const name = newDeckName.trim();
    if (name === "" || busy.current) return;
    busy.current = true;
    try {
      const id = await ipc().createDeck(name);
      setNewDeckName("");
      await loadDecks();
      announce(`Created deck "${name}".`);
      setSelected(id);
    } catch (e) {
      setError(String(e));
    } finally {
      busy.current = false;
    }
  }, [announce, loadDecks, newDeckName]);

  const commitRename = useCallback(async () => {
    if (!renaming || busy.current) return;
    const name = renaming.name.trim();
    if (name === "") {
      setRenaming(null);
      return;
    }
    busy.current = true;
    try {
      await ipc().renameDeck(renaming.id, name);
      setRenaming(null);
      await loadDecks();
    } catch (e) {
      setError(String(e));
    } finally {
      busy.current = false;
    }
  }, [loadDecks, renaming]);

  const confirmDelete = useCallback(async () => {
    if (!deleteTarget || busy.current) return;
    busy.current = true;
    try {
      const affected = await ipc().deleteDeck(deleteTarget.id, deleteMode);
      announce(
        deleteMode === "move"
          ? `Moved ${affected} card(s) to Default and deleted "${deleteTarget.name}".`
          : `Deleted "${deleteTarget.name}" and ${affected} card(s).`,
      );
      setDeleteTarget(null);
      if (selected === deleteTarget.id) setSelected(null);
      await loadDecks();
      if (selected === deleteTarget.id) await loadCards(null);
    } catch (e) {
      setError(String(e));
    } finally {
      busy.current = false;
    }
  }, [announce, deleteMode, deleteTarget, loadCards, loadDecks, selected]);

  const addCard = useCallback(async () => {
    if (!selected || newFront.trim() === "" || newBack.trim() === "") return;
    try {
      await ipc().addCard(selected, newFront.trim(), newBack.trim());
      setNewFront("");
      setNewBack("");
      await loadCards(selected);
      await loadDecks();
    } catch (e) {
      setError(String(e));
    }
  }, [loadCards, loadDecks, newBack, newFront, selected]);

  const saveEdit = useCallback(async () => {
    if (!editing) return;
    try {
      await ipc().updateCard(editing.id, editing.front, editing.back);
      setEditing(null);
      await loadCards(selected);
    } catch (e) {
      setError(String(e));
    }
  }, [editing, loadCards, selected]);

  const removeCard = useCallback(
    async (cardId: string) => {
      if (busy.current) return;
      busy.current = true;
      try {
        await ipc().deleteCard(cardId);
        await loadCards(selected);
        await loadDecks();
      } catch (e) {
        setError(String(e));
      } finally {
        busy.current = false;
      }
    },
    [loadCards, loadDecks, selected],
  );

  const bulkDelete = useCallback(async () => {
    if (busy.current || checked.size === 0) return;
    busy.current = true;
    try {
      for (const id of checked) await ipc().deleteCard(id);
      await loadCards(selected);
      await loadDecks();
    } catch (e) {
      setError(String(e));
    } finally {
      busy.current = false;
    }
  }, [checked, loadCards, loadDecks, selected]);

  const toggleSuspend = useCallback(
    async (card: CardRow) => {
      try {
        await ipc().suspendCard(card.id, !card.suspended);
        await loadCards(selected);
      } catch (e) {
        setError(String(e));
      }
    },
    [loadCards, selected],
  );

  const toggleBury = useCallback(
    async (card: CardRow) => {
      try {
        await ipc().buryCard(card.id, card.buried_until > 0 ? 0 : BURY_HOURS);
        await loadCards(selected);
      } catch (e) {
        setError(String(e));
      }
    },
    [loadCards, selected],
  );

  const saveTags = useCallback(
    async (cardId: string) => {
      const draft = tagDrafts[cardId];
      if (draft === undefined) return;
      const tags = draft
        .split(",")
        .map((t) => t.trim())
        .filter((t) => t !== "");
      try {
        await ipc().setCardTags(cardId, tags);
        await loadCards(selected);
        setTagDrafts((d) => {
          const next = { ...d };
          delete next[cardId];
          return next;
        });
      } catch (e) {
        setError(String(e));
      }
    },
    [loadCards, selected, tagDrafts],
  );

  const doExport = useCallback(async () => {
    const path = await ipc().pickSavePath({
      title: "Export cards",
      defaultName: "olp-export.json",
      extensions: ["json"],
    });
    if (!path) return;
    try {
      const result = await ipc().exportData({
        ...(selected ? { deckId: selected } : {}),
        path,
        format: "json",
      });
      announce(`Exported ${result.cards} card(s) to ${result.path}.`);
    } catch (e) {
      setError(String(e));
    }
  }, [announce, selected]);

  const doImport = useCallback(async () => {
    const path = await ipc().pickOpenPath({
      title: "Import cards",
      extensions: ["json", "csv", "tsv"],
    });
    if (!path) return;
    try {
      const result = await ipc().importData({
        path,
        ...(selected ? { deckId: selected } : {}),
      });
      announce(
        `Imported ${result.cards_created} new and updated ${result.cards_updated} card(s)` +
          (result.skipped > 0 ? `, skipped ${result.skipped}.` : "."),
      );
      await loadDecks();
      await loadCards(selected);
    } catch (e) {
      setError(String(e));
    }
  }, [announce, loadCards, loadDecks, selected]);

  return (
    <section class="route route-wide">
      <div class="route-head">
        <h1 tabIndex={-1}>Decks</h1>
        <div class="row">
          <button type="button" onClick={() => void doImport()}>
            Import
          </button>
          <button type="button" onClick={() => void doExport()}>
            Export
          </button>
        </div>
      </div>

      {error && (
        <p class="notice notice-error" role="alert">
          {error}
        </p>
      )}

      <div class="row">
        <input
          type="text"
          placeholder="New deck name"
          value={newDeckName}
          onInput={(e) => setNewDeckName((e.target as HTMLInputElement).value)}
          onKeyDown={(e) => e.key === "Enter" && void createDeck()}
        />
        <button type="button" onClick={() => void createDeck()}>
          Create deck
        </button>
      </div>

      <ul class="deck-list">
        <li>
          <button
            type="button"
            class={selected === null ? "deck-item deck-item-active" : "deck-item"}
            onClick={() => setSelected(null)}
          >
            All decks
          </button>
        </li>
        {decks.map((d) => (
          <li key={d.id}>
            {renaming?.id === d.id ? (
              <div class="row">
                <input
                  type="text"
                  value={renaming.name}
                  onInput={(e) =>
                    setRenaming({ id: d.id, name: (e.target as HTMLInputElement).value })
                  }
                  onKeyDown={(e) => e.key === "Enter" && void commitRename()}
                />
                <button type="button" onClick={() => void commitRename()}>
                  Save
                </button>
                <button type="button" onClick={() => setRenaming(null)}>
                  Cancel
                </button>
              </div>
            ) : (
              <div class="row">
                <button
                  type="button"
                  class={selected === d.id ? "deck-item deck-item-active" : "deck-item"}
                  onClick={() => setSelected(d.id)}
                >
                  {d.name}
                  <span class="muted">
                    {" "}
                    · {d.card_count} cards, {d.due_count} due, {d.new_count} new
                  </span>
                </button>
                <button type="button" onClick={() => setRenaming({ id: d.id, name: d.name })}>
                  Rename
                </button>
                <button type="button" onClick={() => setDeleteTarget(d)}>
                  Delete
                </button>
              </div>
            )}
          </li>
        ))}
      </ul>

      {deleteTarget && (
        <article class="confirm-panel" role="alertdialog" aria-label="Confirm deck delete">
          <p>
            Delete "{deleteTarget.name}"? It has {deleteTarget.card_count} card(s).
          </p>
          <div class="row" role="radiogroup" aria-label="What to do with its cards">
            <label>
              <input
                type="radio"
                name="delete-mode"
                checked={deleteMode === "move"}
                onChange={() => setDeleteMode("move")}
              />
              Move its cards to Default
            </label>
            <label>
              <input
                type="radio"
                name="delete-mode"
                checked={deleteMode === "delete"}
                onChange={() => setDeleteMode("delete")}
              />
              Delete its cards too
            </label>
          </div>
          <div class="row">
            <button type="button" class="primary" onClick={() => void confirmDelete()}>
              Confirm delete
            </button>
            <button type="button" onClick={() => setDeleteTarget(null)}>
              Cancel
            </button>
          </div>
        </article>
      )}

      <h2>Cards{selected && ` in ${decks.find((d) => d.id === selected)?.name ?? ""}`}</h2>

      {loading && <p class="muted">Loading cards…</p>}

      {!loading && selected && (
        <div class="row">
          <input
            type="text"
            placeholder="Front"
            value={newFront}
            onInput={(e) => setNewFront((e.target as HTMLInputElement).value)}
          />
          <input
            type="text"
            placeholder="Back"
            value={newBack}
            onInput={(e) => setNewBack((e.target as HTMLInputElement).value)}
          />
          <button type="button" onClick={() => void addCard()}>
            Add card
          </button>
        </div>
      )}

      {checked.size > 0 && (
        <div class="row">
          <button type="button" onClick={() => void bulkDelete()}>
            Delete {checked.size} selected
          </button>
        </div>
      )}

      {!loading && cards.length > 0 && (
        <table class="cards-table">
          <caption class="visually-hidden">Cards {selected ? "in this deck" : "in all decks"}</caption>
          <thead>
            <tr>
              <th scope="col">
                <span class="visually-hidden">Select</span>
              </th>
              <th scope="col">Front</th>
              <th scope="col">Back</th>
              <th scope="col">Tags</th>
              <th scope="col">State</th>
              <th scope="col">
                <span class="visually-hidden">Actions</span>
              </th>
            </tr>
          </thead>
          <tbody>
            {cards.map((c) => (
              <tr key={c.id}>
                <td>
                  <input
                    type="checkbox"
                    aria-label={`Select ${c.front}`}
                    checked={checked.has(c.id)}
                    onChange={() =>
                      setChecked((s) => {
                        const next = new Set(s);
                        if (next.has(c.id)) next.delete(c.id);
                        else next.add(c.id);
                        return next;
                      })
                    }
                  />
                </td>
                {editing?.id === c.id ? (
                  <>
                    <td>
                      <input
                        type="text"
                        value={editing.front}
                        onInput={(e) =>
                          setEditing({ ...editing, front: (e.target as HTMLInputElement).value })
                        }
                      />
                    </td>
                    <td>
                      <input
                        type="text"
                        value={editing.back}
                        onInput={(e) =>
                          setEditing({ ...editing, back: (e.target as HTMLInputElement).value })
                        }
                      />
                    </td>
                    <td colSpan={2} />
                    <td>
                      <button type="button" onClick={() => void saveEdit()}>
                        Save
                      </button>
                      <button type="button" onClick={() => setEditing(null)}>
                        Cancel
                      </button>
                    </td>
                  </>
                ) : (
                  <>
                    <td>{c.front}</td>
                    <td>{c.back}</td>
                    <td>
                      <input
                        type="text"
                        aria-label={`Tags for ${c.front}`}
                        value={tagDrafts[c.id] ?? c.tags.join(", ")}
                        onInput={(e) =>
                          setTagDrafts((d) => ({ ...d, [c.id]: (e.target as HTMLInputElement).value }))
                        }
                        onBlur={() => void saveTags(c.id)}
                      />
                    </td>
                    <td>
                      {c.suspended && "Suspended "}
                      {c.buried_until > 0 && "Buried"}
                      {!c.suspended && c.buried_until === 0 && <span class="muted">Active</span>}
                    </td>
                    <td>
                      <button type="button" onClick={() => setEditing({ id: c.id, front: c.front, back: c.back })}>
                        Edit
                      </button>
                      <button type="button" onClick={() => void toggleSuspend(c)}>
                        {c.suspended ? "Unsuspend" : "Suspend"}
                      </button>
                      <button type="button" onClick={() => void toggleBury(c)}>
                        {c.buried_until > 0 ? "Unbury" : "Bury"}
                      </button>
                      <button type="button" onClick={() => void removeCard(c.id)}>
                        Delete
                      </button>
                    </td>
                  </>
                )}
              </tr>
            ))}
          </tbody>
        </table>
      )}

      {!loading && cards.length === 0 && (
        <p class="muted">No cards {selected ? "in this deck" : "yet"}.</p>
      )}
    </section>
  );
}
