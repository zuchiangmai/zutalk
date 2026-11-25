import type { Health } from '$proto/Health';
import type { Stage } from '$proto/Stage';
import type { Tally } from '$proto/Tally';
import type { Water } from '$proto/Water';
import { t, word } from '../bus/words';
import { Icon } from '../ds/icons';
import { Pill, Tag } from '../ds/primitives';
import { healthView } from '../ds/stateView';

function stageKey(stage: Stage): string {
  if (typeof stage === 'string') return stage;
  if ('Sent' in stage) return `Sent:${stage.Sent}`;
  if ('Heard' in stage) return `Heard:${stage.Heard}`;
  return `Final:${stage.Final}`;
}

export function formatStage(stage: Stage): string {
  if (typeof stage === 'string') return stage;
  if ('Sent' in stage) return `Sent #${stage.Sent}`;
  if ('Heard' in stage) return `Heard #${stage.Heard}`;
  return `Final #${stage.Final}`;
}

export type HealthOverlayProps = {
  open: boolean;
  water: Water | null;
  stats: Tally | null;
  onClose: () => void;
  onDiagnose: () => void;
};

/** Dev-mode runtime health overlay surfacing Water stage watermarks and Stats counters (PLAN.md §4.5.2). */
export function HealthOverlay({ open, water, stats, onClose, onDiagnose }: HealthOverlayProps) {
  if (!open) return null;

  const healthByStage = new Map<string, Health>();
  if (water) {
    for (const [stage, health] of water.health) {
      healthByStage.set(stageKey(stage), health);
    }
  }

  return (
    <section className="health-overlay" role="region" aria-label={t('overlay.title')}>
      <div className="health-head">
        <Icon name="activity" size="s" />
        <strong style={{ fontSize: 13 }}>{t('overlay.title')}</strong>
        <span className="spacer" />
        <Pill onClick={onDiagnose}>{t('diagnose')}</Pill>
        <button type="button" className="icon-btn" aria-label={t('close')} onClick={onClose}>
          <Icon name="x" size="s" />
        </button>
      </div>

      {!water && !stats && <div className="k">{t('overlay.empty')}</div>}

      {water && (
        <div>
          <div className="h" style={{ marginBottom: 6 }}>
            {t('overlay.water')}
          </div>
          <table className="health-table">
            <thead>
              <tr>
                <th>Stage</th>
                <th>At</th>
                <th>Silent</th>
                <th>Health</th>
                <th>Why</th>
              </tr>
            </thead>
            <tbody>
              {water.marks.map((mark) => {
                const key = stageKey(mark.stage);
                const verdict = healthByStage.get(key) ?? { is: 'Live' as const };
                const view = healthView(verdict);
                const silentMs = Math.max(0, water.now_ms - mark.moved_ms);
                const why = view.whyCode || mark.why || mark.ended || '';
                return (
                  <tr key={key}>
                    <td className="mono">{formatStage(mark.stage)}</td>
                    <td className="num">{mark.at}</td>
                    <td className="num">{silentMs} ms</td>
                    <td>
                      <Tag tone={view.tone}>{t(view.labelKey)}</Tag>
                    </td>
                    <td>{why ? word('code', why) : '—'}</td>
                  </tr>
                );
              })}
            </tbody>
          </table>
        </div>
      )}

      {stats && stats.queues.length > 0 && (
        <div>
          <div className="h" style={{ marginBottom: 6 }}>
            {t('overlay.queues')}
          </div>
          <table className="health-table">
            <thead>
              <tr>
                <th>Queue</th>
                <th>Depth</th>
                <th>Peak</th>
                <th>Full</th>
                <th>Lost</th>
              </tr>
            </thead>
            <tbody>
              {stats.queues.map((q) => (
                <tr key={q.name}>
                  <td className="mono">{q.name}</td>
                  <td className="num">
                    {q.depth}/{q.capacity}
                  </td>
                  <td className="num">{q.deepest}</td>
                  <td className="num">
                    {q.full > 0 ? <Tag tone="danger">{q.full}</Tag> : 0}
                  </td>
                  <td className="num">
                    {q.lost > 0 ? <Tag tone="danger">{q.lost}</Tag> : 0}
                  </td>
                </tr>
              ))}
            </tbody>
          </table>
        </div>
      )}

      {stats && stats.spreads.length > 0 && (
        <div>
          <div className="h" style={{ marginBottom: 6 }}>
            {t('overlay.spreads')}
          </div>
          <table className="health-table">
            <thead>
              <tr>
                <th>Metric</th>
                <th>p50</th>
                <th>p99</th>
                <th>Max</th>
              </tr>
            </thead>
            <tbody>
              {stats.spreads.map((sp) => (
                <tr key={sp.name}>
                  <td className="mono">{sp.name}</td>
                  <td className="num">{sp.p50}</td>
                  <td className="num">{sp.p99}</td>
                  <td className="num">{sp.most}</td>
                </tr>
              ))}
            </tbody>
          </table>
        </div>
      )}

      {stats && stats.counts.length > 0 && (
        <div>
          <div className="h" style={{ marginBottom: 6 }}>
            {t('overlay.counts')}
          </div>
          <div className="health-chips">
            {stats.counts.map((c) => (
              <Tag key={c.name}>
                <span className="mono">{c.name}</span>: <span className="num">{c.n}</span>
              </Tag>
            ))}
          </div>
        </div>
      )}
    </section>
  );
}
