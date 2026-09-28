import axe from "axe-core";

/**
 * Accessibility gate for component tests.
 *
 * Runs axe-core over a rendered container in jsdom and fails with a list a
 * human can act on: the rule, what it wants, and the offending markup.
 *
 * Only rules jsdom cannot evaluate are switched off. Anything else axe
 * reports is a real finding to fix in the component, not a rule to disable.
 */
const JSDOM_CANNOT_EVALUATE = {
  // jsdom applies no stylesheet-driven paint, so axe cannot resolve the
  // foreground/background pair; the token pairs are checked by
  // scripts/contrast.mjs (and its test) instead.
  "color-contrast": { enabled: false },
} as const;

const SNIPPET_MAX = 120;

function trim(html: string): string {
  const flat = html.replace(/\s+/g, " ").trim();
  return flat.length > SNIPPET_MAX ? `${flat.slice(0, SNIPPET_MAX - 1)}…` : flat;
}

export async function expectNoA11yViolations(container: Element): Promise<void> {
  const results = await axe.run(container, {
    rules: JSDOM_CANNOT_EVALUATE,
    resultTypes: ["violations"],
  });
  if (results.violations.length === 0) return;

  const lines: string[] = [];
  for (const v of results.violations) {
    lines.push(`${v.id} (${v.impact ?? "n/a"}): ${v.help}`);
    for (const node of v.nodes) {
      lines.push(`    ${trim(node.html)}`);
    }
  }
  throw new Error(
    `${results.violations.length} accessibility violation(s):\n${lines.join("\n")}`,
  );
}
