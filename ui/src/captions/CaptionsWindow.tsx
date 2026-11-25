import { frameSource, stateSource, useSource } from '../bus';

/** Minimal captions window mount stub for P3-01; completed in P3-06. */
export function CaptionsWindow() {
  const state = useSource(stateSource);
  const frame = useSource(frameSource);
  return (
    <div className="zt" data-window="captions" data-mode={state?.setup.mode ?? 'Audience'}>
      <main>{frame ? `${frame.columns.length} columns` : 'Listening…'}</main>
    </div>
  );
}
