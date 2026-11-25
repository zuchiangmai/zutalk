import type { ReactNode } from 'react';
import type { HostState } from '$proto/HostState';
import type { Session } from '$proto/Session';
import { t } from '../bus/words';
import { Icon } from '../ds/icons';
import { Button, Pill, Tag } from '../ds/primitives';

export type TopBarProps = {
  host: HostState;
  session?: Session | null;
  update: string | null;
  recording: boolean;
  onUpdate: () => void;
  onStopOutbound?: () => void;
  onCancelHost?: () => void;
  onCopyHostLink?: (link: string) => void;
  onShowHostQr?: () => void;
  children?: ReactNode;
};

/** Paper surface shows local-only badge; inverse `.band` appears only when data leaves the device (`HostLive.dc.html`). */
export function TopBar({
  host,
  session = null,
  update,
  recording,
  onUpdate,
  onStopOutbound,
  onCancelHost,
  onCopyHostLink,
  onShowHostQr,
  children,
}: TopBarProps) {
  const isLiveOutbound = Boolean(session?.outbound || (host.is === 'Live' && host.outbound));
  const isArmedOutbound = !session && host.is === 'Armed' && host.captions;
  const outbound = isLiveOutbound || isArmedOutbound;

  const viewers = session?.outbound
    ? session.viewers
    : host.is === 'Armed' || host.is === 'Live'
      ? host.viewers
      : 0;
  const eventTitle =
    session?.event_title ??
    (host.is === 'Armed' || host.is === 'Live' ? (host.event?.title ?? null) : null);
  const link = host.is === 'Armed' || host.is === 'Live' ? host.link : null;

  if (outbound) {
    return (
      <>
        <header className="band" role="status">
          <Icon name="broadcast" size="s" />
          <b>{isLiveOutbound ? t('outbound.live') : t('host.band.armed')}</b>
          <span className="sep" />
          <span className="chip">
            <Icon name="users" size="s" />
            <span className="num">
              {isLiveOutbound ? t('host.watching', viewers) : t('host.waiting', viewers)}
            </span>
          </span>
          <span>
            {t('host.audience_captions')}
            {eventTitle ? ` · ${eventTitle}` : ''}
          </span>
          <span className="spacer" />
          {link && onCopyHostLink && (
            <Pill onClick={() => onCopyHostLink(link)}>
              <Icon name="copy" size="s" />
              {t('link.copy')}
            </Pill>
          )}
          {link && onShowHostQr && (
            <Pill aria-label={t('qr')} onClick={onShowHostQr}>
              <Icon name="qrcode" size="s" />
            </Pill>
          )}
          {isLiveOutbound && onStopOutbound && (
            <Pill onClick={onStopOutbound}>
              <Icon name="eye-off" size="s" />
              {t('host.stop_outbound')}
            </Pill>
          )}
          {isArmedOutbound && onCancelHost && (
            <Pill onClick={onCancelHost}>{t('host.cancel')}</Pill>
          )}
        </header>
        {(children || (update && !recording)) && (
          <div className="top">
            <span className="spacer" />
            {children}
            {update && !recording && (
              <Button size="sm" onClick={onUpdate}>
                {t('update')}
              </Button>
            )}
          </div>
        )}
      </>
    );
  }

  return (
    <header className="top">
      <span className="private">
        <Icon name="lock" size="s" />
        {t('private.local')}
      </span>
      {eventTitle && <Tag>{t('host.can_file_after', eventTitle)}</Tag>}
      <span className="spacer" />
      {children}
      {update && !recording && (
        <Button size="sm" onClick={onUpdate}>
          {t('update')}
        </Button>
      )}
    </header>
  );
}
