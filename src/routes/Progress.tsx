import { useEffect, useState } from "preact/hooks";

import { requestPractice } from "../app/handoff";
import { navigate } from "../app/router";
import { ipc } from "../ipc/commands";
import type { AttemptRow, DayCount, ForecastDay, Overview, PracticeDay, RetentionBucket } from "../ipc/types";
import { BarChart, LineChart, type ChartPoint } from "../components/Chart";
import { tzOffsetMinutes } from "../lib/tz";
import { describeWeek } from "../lib/weeklyRecap";

const STATS_DAYS = 30;
const FORECAST_DAYS = 14;
const RETENTION_DAYS = 90;
const RETENTION_BUCKET_DAYS = 7;
/** Newest practice attempts listed; the backend caps the request at 200. */
const RECENT_ATTEMPTS = 20;
/** Characters of a free-speaking transcript shown in the history table. */
const SNIPPET_CHARS = 60;

/** `unix seconds -> "Mon 3"`, short enough for a chart axis and a table cell. */
function dayLabel(unixSeconds: number): string {
  const d = new Date(unixSeconds * 1000);
  return d.toLocaleDateString(undefined, { weekday: "short", day: "numeric" });
}

function minutes(ms: number): number {
  return Math.round(ms / 60_000);
}

/** `created_at` is unix milliseconds. */
function whenLabel(ms: number): string {
  return new Date(ms).toLocaleString(undefined, {
    weekday: "short",
    day: "numeric",
    month: "short",
    hour: "numeric",
    minute: "2-digit",
  });
}

function snippet(text: string): string {
  const t = text.trim();
  return t.length > SNIPPET_CHARS ? `${t.slice(0, SNIPPET_CHARS - 1)}…` : t;
}

export function Progress() {
  const [overview, setOverview] = useState<Overview | null>(null);
  const [daily, setDaily] = useState<DayCount[]>([]);
  const [practice, setPractice] = useState<PracticeDay[]>([]);
  const [forecast, setForecast] = useState<ForecastDay[]>([]);
  const [retention, setRetention] = useState<RetentionBucket[]>([]);
  const [attempts, setAttempts] = useState<AttemptRow[] | null>(null);
  const [attemptsError, setAttemptsError] = useState<string | null>(null);
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
    // Separate from the stats above, so a failure here does not blank the charts.
    void (async () => {
      try {
        const rows = await ipc().statsPractice(STATS_DAYS, tzOffsetMinutes());
        if (live) setPractice(rows);
      } catch {
        // Only the speaking charts are lost; the rest of the screen stands.
      }
    })();
    void (async () => {
      try {
        const rows = await ipc().listAttempts(undefined, RECENT_ATTEMPTS);
        if (live) setAttempts(rows);
      } catch (e) {
        if (live) setAttemptsError(String(e));
      }
    })();
    return () => {
      live = false;
    };
  }, []);

  const dailyPoints: ChartPoint[] = daily.map((d) => ({ label: dayLabel(d.day), value: d.reviews }));
  const practicePoints: ChartPoint[] = practice.map((p) => ({ label: dayLabel(p.day), value: p.attempts }));
  // Only days with an acoustic score: a day without one is a gap, not a zero,
  // so it is left off rather than drawn as a drop.
  const pronPoints: ChartPoint[] = practice.flatMap((p) =>
    p.avg_pron === null ? [] : [{ label: dayLabel(p.day), value: Math.round(p.avg_pron) }],
  );
  const weekNote = describeWeek(practice);
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
          <Tile label="Speaking today" value={String(overview.attempts_today)} />
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

      {weekNote && <p class="week-recap">{weekNote}</p>}

      {!loading && (
        <div class="charts">
          <BarChart title="Reviews per day (last 30 days)" unit="Reviews" data={dailyPoints} />
          <BarChart title="Speaking attempts per day (last 30 days)" unit="Attempts" data={practicePoints} />
          <LineChart title="Pronunciation on days you read aloud" unit="Average score" data={pronPoints} />
          <p class="muted chart-note">
            Days without a read-aloud attempt are left out. The score is measured against expert ratings of
            speakers whose first language is Mandarin, so treat it as a hint.
          </p>
          <BarChart title="Due forecast (next 14 days)" unit="Cards due" data={forecastPoints} />
          <LineChart title="Retention by week" unit="Retention %" data={retentionPoints} />
        </div>
      )}

      <RecentPractice attempts={attempts} error={attemptsError} />
    </section>
  );
}

function RecentPractice(props: { attempts: AttemptRow[] | null; error: string | null }) {
  const { attempts, error } = props;
  return (
    <section class="recent-practice" aria-labelledby="recent-practice-title">
      <h2 id="recent-practice-title">Recent practice</h2>
      {error && (
        <p class="notice notice-error" role="alert">
          Could not load your practice history: {error}
        </p>
      )}
      {!error && attempts === null && <p class="muted">Loading…</p>}
      {attempts?.length === 0 && (
        <p class="muted">No practice yet. Record an attempt on the Practice screen and it shows up here.</p>
      )}
      {attempts && attempts.length > 0 && (
        <table class="cards-table">
          <thead>
            <tr>
              <th scope="col">When</th>
              <th scope="col">What you practised</th>
              <th scope="col">Overall</th>
              <th scope="col">Pronunciation</th>
              <th scope="col">Length</th>
              <th scope="col">
                <span class="visually-hidden">Practise again</span>
              </th>
            </tr>
          </thead>
          <tbody>
            {attempts.map((a) => (
              <tr key={a.id}>
                <td>{whenLabel(a.created_at)}</td>
                <td>
                  {a.target_text ?? (
                    <>
                      <span class="muted">Free speaking:</span> {snippet(a.transcript)}
                    </>
                  )}
                </td>
                <td>{a.overall}</td>
                <td>
                  {/* Free speaking has no pronunciation score; never show one. */}
                  {a.pron_overall === null ? (
                    <span class="muted">Not scored</span>
                  ) : a.pron_method === "text" ? (
                    <>
                      {a.pron_overall} <span class="muted">(word matching)</span>
                    </>
                  ) : (
                    a.pron_overall
                  )}
                </td>
                <td>{(a.duration_ms / 1000).toFixed(1)}s</td>
                <td>
                  {a.prompt_id !== null && (
                    <button
                      type="button"
                      onClick={() => {
                        requestPractice(a.prompt_id as string);
                        navigate("practice");
                      }}
                    >
                      Practise again
                    </button>
                  )}
                </td>
              </tr>
            ))}
          </tbody>
        </table>
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
