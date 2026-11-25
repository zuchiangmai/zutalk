import type { HostState } from '$proto/HostState';
import type { OutboundRow } from '$proto/OutboundRow';
import type { Session } from '$proto/Session';
import type { Writing } from '$proto/Writing';
import {
  go,
  langSource,
  progressSource,
  send,
  sessionSource,
  stateSource,
  t,
  useQuery,
  useSource,
  word,
} from '../bus';
import { Icon } from '../ds/icons';
import { Pill, Tag } from '../ds/primitives';

export type OutboundViewProps = {
  host: HostState;
  session: Session | null;
  rows: readonly OutboundRow[];
  progressFile: Writing | null;
  onBack: () => void;
  onStopOutbound: () => void;
  onOpenRecording: (id: string) => void;
};

/** Pure props Settings -> Outbound overview (`Outbound.dc.html`): live outbound status + city archive filings. */
export function OutboundView({
  host,
  session,
  rows,
  progressFile,
  onBack,
  onStopOutbound,
  onOpenRecording,
}: OutboundViewProps) {
  const liveOutbound = Boolean(
    session?.outbound || (host.is === 'Live' && host.outbound) || (host.is === 'Armed' && host.captions),
  );
  const viewers = session?.outbound
    ? session.viewers
    : host.is === 'Live' || host.is === 'Armed'
      ? host.viewers
      : 0;
  const eventTitle =
    session?.event_title ??
    (host.is === 'Live' || host.is === 'Armed' ? (host.event?.title ?? null) : null);

  return (
    <div className="main" style={{ flex: 1 }}>
      <header className="top">
        <Pill quiet onClick={onBack}>
          <Icon name="chevron-left" size="s" />
          {t('nav.settings')}
        </Pill>
      </header>

      <div className="page">
        <h1 className="page-t">{t('outbound.title')}</h1>

        <section className="group">
          <h2 className="h">{t('outbound.live_section')}</h2>
          {liveOutbound ? (
            <div className="line" role="status">
              <span className="dot live" />
              <Tag tone="out">
                <Icon name="broadcast" size="s" />
                {t('outbound.live')}
              </Tag>
              <span className="num">
                {session ? t('host.watching', viewers) : t('host.waiting', viewers)}
              </span>
              {eventTitle && <span className="k">{eventTitle}</span>}
              <span className="spacer" />
              <Pill onClick={onStopOutbound}>
                <Icon name="eye-off" size="s" />
                {t('host.stop_outbound')}
              </Pill>
            </div>
          ) : (
            <div className="line" role="status">
              <span className="dot" />
              <span>{t('outbound.none_live')}</span>
            </div>
          )}
        </section>

        <section className="group">
          <h2 className="h">{t('outbound.archive_section')}</h2>
          {rows.length === 0 ? (
            <p className="k">{t('outbound.archive_empty')}</p>
          ) : (
            rows.map((row) => {
              const activeProg = progressFile?.id === row.id ? progressFile : null;
              const isGoing = activeProg !== null || row.filing.state === '';
              const isFiled = !isGoing && row.filing.state === 'city.filed';
              const isFailed = !isGoing && !isFiled;
              const pct =
                activeProg && activeProg.of > 0
                  ? Math.min(100, Math.round((activeProg.done / activeProg.of) * 100))
                  : 33;
              const eventMeta = [row.filing.city, row.filing.title].filter(Boolean).join(' · ');
              const whatSummary = row.filing.audio
                ? t('file.summary_with_audio')
                : t('file.summary_text_only');

              return (
                <button
                  key={`${row.id}:${row.filing.key}`}
                  type="button"
                  className="outbound-row"
                  onClick={() => onOpenRecording(row.id)}
                >
                  <span
                    className="read grow"
                    style={{
                      fontSize: 15,
                      overflow: 'hidden',
                      textOverflow: 'ellipsis',
                      whiteSpace: 'nowrap',
                    }}
                  >
                    {row.title || t('untitled')}
                  </span>
                  {eventMeta && <span className="k">{eventMeta}</span>}
                  {isFiled && (
                    <>
                      <Tag tone="out">
                        <Icon name="arrow-up-right" size="s" />
                        {t('file.filed_badge')}
                      </Tag>
                      <Tag>{whatSummary}</Tag>
                    </>
                  )}
                  {isGoing && (
                    <>
                      <span className="bar" style={{ width: 56 }}>
                        <i style={{ width: `${pct}%` }} />
                      </span>
                      <span className="shimmer" style={{ fontSize: 12 }}>
                        {activeProg?.step === 'audio'
                          ? t('file.uploading_audio')
                          : t('file.submitting')}
                      </span>
                    </>
                  )}
                  {isFailed && (
                    <>
                      <Tag tone="warn">{t('file.failed_badge')}</Tag>
                      <span className="k">{word('code', row.filing.state)}</span>
                    </>
                  )}
                </button>
              );
            })
          )}
        </section>
      </div>
    </div>
  );
}

/** Bus-connected OutboundPage container (`Query::Outbound` + live status). */
export function OutboundPage() {
  const state = useSource(stateSource);
  const session = useSource(sessionSource);
  const progress = useSource(progressSource);
  useSource(langSource);
  const outboundQuery = useQuery({ view: 'Outbound' });

  return (
    <OutboundView
      host={state?.host ?? { is: 'Idle' }}
      session={session}
      rows={outboundQuery.data ?? []}
      progressFile={progress.file}
      onBack={() => go({ page: 'settings' })}
      onStopOutbound={() => void send({ do: 'StopOutbound' })}
      onOpenRecording={(id) => go({ page: 'reader', id })}
    />
  );
}
