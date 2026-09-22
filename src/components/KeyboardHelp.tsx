import { KEY_HELP } from "../lib/globalKeys";

/**
 * The keyboard map, on screen.
 *
 * Shortcuts nobody can discover are shortcuts nobody uses, and this app is
 * meant to be driven from the keyboard while the learner is looking at the
 * prompt rather than the mouse.
 */
export function KeyboardHelp(props: { onClose: () => void }) {
  return (
    <div class="sheet-backdrop" onClick={props.onClose}>
      <div
        class="sheet"
        role="dialog"
        aria-modal="true"
        aria-label="Keyboard shortcuts"
        onClick={(e) => e.stopPropagation()}
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
        <button type="button" class="primary" onClick={props.onClose} autofocus>
          Close
        </button>
      </div>
    </div>
  );
}
