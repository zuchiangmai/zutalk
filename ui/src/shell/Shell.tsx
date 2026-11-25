import { routeSource, stateSource, useSource } from '../bus';

/** Minimal shell mount stub for P3-01; replaced with full two-column shell in P3-02. */
export function Shell() {
  const state = useSource(stateSource);
  const route = useSource(routeSource);
  return (
    <div className="zt" data-page={route.page}>
      <main>{state ? 'ZuTalk' : 'Loading…'}</main>
    </div>
  );
}
