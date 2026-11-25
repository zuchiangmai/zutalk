import { useSource } from '../bus';
import { roomLiveSource, roomOpenSource } from './room';

/** Minimal viewer page mount stub for P3-01; completed in P3-06. */
export function ViewerPage() {
  const live = useSource(roomLiveSource);
  const open = useSource(roomOpenSource);
  return (
    <div className="zt" data-window="viewer">
      <main>{open ? open.row.title : `${live.length} live`}</main>
    </div>
  );
}
