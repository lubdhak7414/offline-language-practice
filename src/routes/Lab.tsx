import { useEffect, useRef } from "preact/hooks";

/**
 * The original developer harness, kept reachable while the real screens are
 * built.
 *
 * It is imperative DOM code driving hand-written markup, so rather than
 * rewriting it twice it is mounted as-is and torn down on unmount. It goes
 * away once Decks, Progress and Settings exist.
 */
export function Lab() {
  const host = useRef<HTMLDivElement>(null);
  const mounted = useRef(false);

  useEffect(() => {
    if (mounted.current || !host.current) return;
    mounted.current = true;
    const template = document.getElementById("lab-template");
    if (template instanceof HTMLTemplateElement) {
      host.current.appendChild(template.content.cloneNode(true));
    }
    // Dynamic import so the harness only loads when the route is opened,
    // and `mountLab` runs after the markup is in the DOM.
    void import("../main").then((m) => m.mountLab());
  }, []);

  return (
    <section class="route">
      <h1 tabIndex={-1}>Lab</h1>
      <p class="muted">
        The original test harness. Every control here will move into a real
        screen before release.
      </p>
      <div ref={host} />
    </section>
  );
}
