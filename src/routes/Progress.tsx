import { useEffect, useState } from "preact/hooks";

import { ipc } from "../ipc/commands";
import type { DayCount, ForecastDay, Overview, RetentionBucket } from "../ipc/types";
import { BarChart, LineChart, type ChartPoint } from "../components/Chart";
import { tzOffsetMinutes } from "../lib/tz";

const STATS_DAYS = 30;
const FORECAST_DAYS = 14;
const RETENTION_DAYS = 90;
const RETENTION_BUCKET_DAYS = 7;

/** `unix seconds -> "Mon 3"`, short enough for a chart axis and a table cell. */
function dayLabel(unixSeconds: number): string {
  const d = new Date(unixSeconds * 1000);
  return d.toLocaleDateString(undefined, { weekday: "short", day: "numeric" });
}

function minutes(ms: number): number {
  return Math.round(ms / 60_000);
}

export function Progress() {
  const [overview, setOverview] = useState<Overview | null>(null);
  const [daily, setDaily] = useState<DayCount[]>([]);
  const [forecast, setForecast] = useState<ForecastDay[]>([]);
  const [retention, setRetention] = useState<RetentionBucket[]>([]);
  const [error, setError] = useState<string | null>(null);
  const [loading, setLoading] = useState(true);

  useEffect(() => {
    let live = true;
    void (async () => {
      const tz = tzOffsetMinutes();
      try {
        const [ov, d, f, r] = await Promise.all([
          ipc().statsOverview(tz),
          ipc().statsDaily(STATS_DAYS, tz),
          ipc().statsForecast(FORECAST_DAYS, tz),
          ipc().statsRetention(RETENTION_DAYS, RETENTION_BUCKET_DAYS, tz),
        ]);
        if (!live) return;
        setOverview(ov);
        setDaily(d);
        setForecast(f);
        setRetention(r);
      } catch (e) {
        if (live) setError(String(e));
      } finally {
        if (live) setLoading(false);
      }
    })();
    return () => {
      live = false;
    };
  }, []);

  const dailyPoints: ChartPoint[] = daily.map((d) => ({ label: dayLabel(d.day), value: d.reviews }));
  const forecastPoints: ChartPoint[] = forecast.map((f) => ({ label: dayLabel(f.day), value: f.due }));
  const retentionPoints: ChartPoint[] = retention.map((r) => ({
    label: dayLabel(r.day),
    value: Math.round(r.rate * 100),
  }));

  return (
    <section class="route route-wide">
      <h1 tabIndex={-1}>Progress</h1>

      {error && (
        <p class="notice notice-error" role="alert">
          {error}
        </p>
      )}
      {loading && <p class="muted">Loading your progress…</p>}

      {overview && (
        <div class="tiles">
          <Tile label="Streak" value={`${overview.streak_days} ${overview.streak_days === 1 ? "day" : "days"}`} />
          <Tile label="Reviews today" value={String(overview.reviews_today)} />
          <Tile label="Total reviews" value={String(overview.total_reviews)} />
          <Tile label="Mature cards" value={String(overview.cards_mature)} />
          <Tile
            label="30-day retention"
            value={
              overview.retention_30d === null
                ? "Not enough data"
                : `${Math.round(overview.retention_30d * 100)}%`
            }
          />
          <Tile label="Practice time" value={`${minutes(overview.practice_ms_30d)} min`} />
        </div>
      )}

      {!loading && (
        <div class="charts">
          <BarChart title="Reviews per day (last 30 days)" unit="Reviews" data={dailyPoints} />
          <BarChart title="Due forecast (next 14 days)" unit="Cards due" data={forecastPoints} />
          <LineChart title="Retention by week" unit="Retention %" data={retentionPoints} />
        </div>
      )}
    </section>
  );
}

function Tile(props: { label: string; value: string }) {
  return (
    <div class="tile">
      <div class="tile-label">{props.label}</div>
      <div class="tile-value">{props.value}</div>
    </div>
  );
}
