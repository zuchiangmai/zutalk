import type { ReactNode } from 'react';
import type { HostState } from '$proto/HostState';
import { t } from '../bus/words';
import { Icon } from '../ds/icons';
import { Button } from '../ds/primitives';

export type TopBarProps = {
  host: HostState;
  update: string | null;
  recording: boolean;
  onUpdate: () => void;
  children?: ReactNode;
};

/** Paper surface shows local-only badge; inverse band appears only when data leaves the device. */
export function TopBar({ host, update, recording, onUpdate, children }: TopBarProps) {
  const outbound =
    (host.is === 'Armed' && host.captions) || (host.is === 'Live' && host.outbound);
  const viewers = host.is === 'Armed' || host.is === 'Live' ? host.viewers : 0;
  return (
    <header className={outbound ? 'band' : 'top'}>
      {outbound ? (
        <>
          <Icon name="broadcast" size="s" />
          <b>{t('outbound.live')}</b>
          <span className="chip">{t('outbound.count', viewers)}</span>
        </>
      ) : (
        <span className="private">
          <Icon name="lock" size="s" />
          {t('private.local')}
        </span>
      )}
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
