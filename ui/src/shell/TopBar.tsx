import type { ReactNode } from 'react';
import type { Room } from '$proto/Room';
import { t } from '../bus/words';
import { Icon } from '../ds/icons';
import { Button } from '../ds/primitives';

export type TopBarProps = {
  rooms: readonly Room[];
  update: string | null;
  recording: boolean;
  onUpdate: () => void;
  children?: ReactNode;
};

/** Paper surface shows local-only badge; inverse band appears only when data leaves the device. */
export function TopBar({ rooms, update, recording, onUpdate, children }: TopBarProps) {
  const outbound = rooms.length > 0;
  return (
    <header className={outbound ? 'band' : 'top'}>
      {outbound ? (
        <>
          <Icon name="broadcast" size="s" />
          <b>{t('outbound.live')}</b>
          <span className="chip">{t('outbound.count', rooms.length)}</span>
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
