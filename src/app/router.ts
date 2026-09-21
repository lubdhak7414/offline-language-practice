import { signal } from "@preact/signals";

/**
 * Hash routing.
 *
 * A desktop app has no server to configure and no URLs to share, so the
 * history API would buy nothing and cost a dev-server rewrite rule. The hash
 * also survives the Tauri custom protocol unchanged.
 */
export const ROUTES = ["practice", "review", "decks", "progress", "lab"] as const;

export type Route = (typeof ROUTES)[number];

export const DEFAULT_ROUTE: Route = "practice";

export function parseRoute(hash: string): Route {
  const name = hash.replace(/^#\/?/, "").split("/")[0] ?? "";
  return (ROUTES as readonly string[]).includes(name) ? (name as Route) : DEFAULT_ROUTE;
}

export const route = signal<Route>(
  parseRoute(typeof location === "undefined" ? "" : location.hash),
);

export function navigate(next: Route) {
  if (typeof location !== "undefined") location.hash = `#/${next}`;
  route.value = next;
}

/** Start syncing `route` with the address bar. Returns a teardown function. */
export function startRouter(): () => void {
  const onChange = () => {
    route.value = parseRoute(location.hash);
  };
  addEventListener("hashchange", onChange);
  onChange();
  return () => removeEventListener("hashchange", onChange);
}
