import { render, screen } from "@testing-library/preact";
import { describe, expect, it } from "vitest";

import { BarChart, barGeometry, linePath } from "./Chart";

describe("barGeometry", () => {
  it("returns nothing for an empty series", () => {
    expect(barGeometry([], 100, 50)).toEqual([]);
  });

  it("gives a single value the full width", () => {
    const [bar] = barGeometry([10], 100, 50, 4);
    expect(bar).toBeDefined();
    expect(bar?.width).toBe(100);
    expect(bar?.height).toBe(50);
    expect(bar?.y).toBe(0);
  });

  it("never divides by zero on an all-zero series", () => {
    const bars = barGeometry([0, 0, 0], 100, 50);
    expect(bars).toHaveLength(3);
    for (const b of bars) {
      expect(b.height).toBe(0);
      expect(b.y).toBe(50);
      expect(Number.isFinite(b.width)).toBe(true);
    }
  });

  it("gives the maximum value the full chart height", () => {
    const bars = barGeometry([2, 10, 4], 100, 50);
    expect(bars[1]?.height).toBe(50);
    expect(bars[1]?.y).toBe(0);
    expect(bars[0]?.height).toBeCloseTo(10);
  });
});

describe("linePath", () => {
  it("returns nothing for an empty series", () => {
    expect(linePath([], 100, 50)).toEqual({ d: "", points: [] });
  });

  it("plots a single point without dividing by zero", () => {
    // One value, scaled against a zero baseline, sits at the top of the
    // chart (it is both the min-relative max and the only point).
    const { points, d } = linePath([5], 100, 50);
    expect(points).toEqual([{ x: 0, y: 0 }]);
    expect(d).toBe("M0.00,0.00");
  });

  it("flattens an all-zero series to the baseline instead of NaN", () => {
    const { points } = linePath([0, 0, 0], 100, 50);
    for (const p of points) {
      expect(Number.isFinite(p.y)).toBe(true);
      expect(p.y).toBe(50);
    }
  });

  it("puts the maximum value at the top", () => {
    const { points } = linePath([0, 10], 100, 50);
    expect(points[1]?.y).toBe(0);
    expect(points[0]?.y).toBe(50);
  });
});

describe("BarChart", () => {
  it("renders an accessible chart with a hidden data table", () => {
    const { container } = render(
      <BarChart
        title="Reviews per day"
        unit="Reviews"
        data={[
          { label: "Mon", value: 4 },
          { label: "Tue", value: 8 },
        ]}
      />,
    );
    expect(screen.getByRole("img", { name: /Reviews per day/ })).toBeInTheDocument();
    expect(container.querySelector("figcaption")).toHaveTextContent("Reviews per day");
    const table = screen.getByRole("table");
    expect(table).toHaveTextContent("Mon");
    expect(table).toHaveTextContent("8");
  });

  it("says so plainly with no data, rather than an empty chart", () => {
    render(<BarChart title="Reviews per day" unit="Reviews" data={[]} />);
    expect(screen.getByText("No data yet.")).toBeInTheDocument();
    expect(screen.queryByRole("img")).not.toBeInTheDocument();
  });
});
