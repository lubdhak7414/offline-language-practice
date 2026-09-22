/**
 * Pure inline SVG charts, no charting dependency.
 *
 * The scale maths (`barGeometry`, `linePath`) are exported separately from
 * the components so they can be unit-tested without a DOM: an empty series,
 * a single point, an all-zero series, and a value sitting at the maximum are
 * exactly the inputs that produce NaN or a divide-by-zero in the naive
 * version, so each is a named test case rather than a spot check.
 *
 * Every chart also renders a visually-hidden `<table>` of the same numbers
 * (`ChartTable` below), so the data is never image-only.
 */

export type ChartPoint = { label: string; value: number };

export type BarGeometry = { x: number; y: number; width: number; height: number };

/** One rectangle per value, scaled to `height`, flush to the baseline. */
export function barGeometry(
  values: number[],
  width: number,
  height: number,
  gap = 4,
): BarGeometry[] {
  if (values.length === 0) return [];
  const max = Math.max(...values, 0);
  const n = values.length;
  const barWidth = Math.max(0, (width - gap * (n - 1)) / n);
  return values.map((v, i) => {
    const h = max <= 0 ? 0 : (Math.max(0, v) / max) * height;
    return {
      x: i * (barWidth + gap),
      y: height - h,
      width: barWidth,
      height: h,
    };
  });
}

export type LinePoint = { x: number; y: number };

/** An SVG path `d` string plus the plotted points, scaled to `width`/`height`. */
export function linePath(
  values: number[],
  width: number,
  height: number,
): { d: string; points: LinePoint[] } {
  if (values.length === 0) return { d: "", points: [] };
  const max = Math.max(...values, 0);
  const min = Math.min(...values, 0);
  // A flat (or single-point) series would divide by zero; fall back to 1 so
  // it renders as a flat line at the baseline instead of NaN everywhere.
  const range = max - min || 1;
  const step = values.length > 1 ? width / (values.length - 1) : 0;
  const points = values.map((v, i) => ({
    x: i * step,
    y: height - ((v - min) / range) * height,
  }));
  const d = points
    .map((p, i) => `${i === 0 ? "M" : "L"}${p.x.toFixed(2)},${p.y.toFixed(2)}`)
    .join(" ");
  return { d, points };
}

const WIDTH = 480;
const HEIGHT = 140;

/** The visually-hidden numeric table every chart carries alongside its SVG. */
function ChartTable(props: { caption: string; data: ChartPoint[]; unit: string }) {
  return (
    <table class="visually-hidden">
      <caption>{props.caption}</caption>
      <thead>
        <tr>
          <th scope="col">Day</th>
          <th scope="col">{props.unit}</th>
        </tr>
      </thead>
      <tbody>
        {props.data.map((d) => (
          <tr key={d.label}>
            <th scope="row">{d.label}</th>
            <td>{d.value}</td>
          </tr>
        ))}
      </tbody>
    </table>
  );
}

export function BarChart(props: { title: string; data: ChartPoint[]; unit: string }) {
  const { title, data, unit } = props;
  const values = data.map((d) => d.value);
  const bars = barGeometry(values, WIDTH, HEIGHT);
  return (
    <figure class="chart">
      <figcaption>{title}</figcaption>
      {data.length === 0 ? (
        <p class="muted">No data yet.</p>
      ) : (
        <svg
          viewBox={`0 0 ${WIDTH} ${HEIGHT}`}
          role="img"
          aria-label={`${title}: ${data.map((d) => `${d.label} ${d.value}`).join(", ")}`}
        >
          {bars.map((b, i) => (
            <rect
              key={data[i]?.label ?? i}
              x={b.x}
              y={b.y}
              width={b.width}
              height={Math.max(b.height, values[i] === 0 ? 0 : 1)}
              class="chart-bar"
            />
          ))}
        </svg>
      )}
      <ChartTable caption={title} data={data} unit={unit} />
    </figure>
  );
}

export function LineChart(props: { title: string; data: ChartPoint[]; unit: string }) {
  const { title, data, unit } = props;
  const values = data.map((d) => d.value);
  const { d, points } = linePath(values, WIDTH, HEIGHT);
  return (
    <figure class="chart">
      <figcaption>{title}</figcaption>
      {data.length === 0 ? (
        <p class="muted">No data yet.</p>
      ) : (
        <svg
          viewBox={`0 0 ${WIDTH} ${HEIGHT}`}
          role="img"
          aria-label={`${title}: ${data.map((p) => `${p.label} ${p.value}`).join(", ")}`}
        >
          <path d={d} class="chart-line" fill="none" />
          {points.map((p, i) => (
            <circle key={data[i]?.label ?? i} cx={p.x} cy={p.y} r={2.5} class="chart-dot" />
          ))}
        </svg>
      )}
      <ChartTable caption={title} data={data} unit={unit} />
    </figure>
  );
}
