import { useEffect, useRef } from "preact/hooks";

import { KEY_HELP } from "../lib/globalKeys";

const FOCUSABLE = 'button, [href], input, select, textarea, [tabindex]:not([tabindex="-1"])';

/**
 * The keyboard map, on screen.
 *
 * Shortcuts nobody can discover are shortcuts nobody uses, and this app is
 * meant to be driven from the keyboard while the learner is looking at the
 * prompt rather than the mouse.
 *
 * A modal in the ARIA sense: focus moves in on open, Tab cannot leave the
 * sheet (it would land on the page behind the backdrop), and closing puts
 * focus back where it was, so a keyboard user does not restart from the top.
 */
export function KeyboardHelp(props: { onClose: () => void }) {
  const sheet = useRef<HTMLDivElement>(null);
  const close = useRef<HTMLButtonElement>(null);

  useEffect(() => {
    const opener = document.activeElement instanceof HTMLElement ? document.activeElement : null;
    close.current?.focus();
    return () => {
      if (opener?.isConnected) opener.focus();
    };
  }, []);

  const trapTab = (e: KeyboardEvent) => {
    if (e.key !== "Tab" || !sheet.current) return;
    const items = Array.from(sheet.current.querySelectorAll<HTMLElement>(FOCUSABLE));
    const first = items[0];
    const last = items[items.length - 1];
    if (!first || !last) return;
    if (e.shiftKey && document.activeElement === first) {
      e.preventDefault();
      last.focus();
    } else if (!e.shiftKey && document.activeElement === last) {
      e.preventDefault();
      first.focus();
    }
  };

  return (
    <div class="sheet-backdrop" onClick={props.onClose}>
      <div
        class="sheet"
        role="dialog"
        aria-modal="true"
        aria-label="Keyboard shortcuts"
        ref={sheet}
        onClick={(e) => e.stopPropagation()}
        onKeyDown={trapTab}
      >
        <h2>Keyboard shortcuts</h2>
        <table class="shortcuts">
          <caption class="visually-hidden">
            Every keyboard shortcut in the app
          </caption>
          <thead>
            <tr>
              <th scope="col">Key</th>
              <th scope="col">Does</th>
            </tr>
          </thead>
          <tbody>
            {KEY_HELP.map((row) => (
              <tr key={row.keys}>
                <th scope="row">
                  <kbd>{row.keys}</kbd>
                </th>
                <td>{row.what}</td>
              </tr>
            ))}
          </tbody>
        </table>
        <button type="button" class="primary" onClick={props.onClose} ref={close}>
          Close
        </button>
      </div>
    </div>
  );
}
