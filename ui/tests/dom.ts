// Shared deterministic jsdom mocks for ResizeObserver, matchMedia, and VList viewport sizing (P3.md §3.5).

export type DomMockOptions = {
  viewportWidth?: number;
  viewportHeight?: number;
  reducedMotion?: boolean;
  darkScheme?: boolean;
};

/** Default simulated viewport dimensions so virtua VList crops rows deterministically in jsdom. */
export const DEFAULT_VIEWPORT_WIDTH_PX = 800;
export const DEFAULT_VIEWPORT_HEIGHT_PX = 600;

export function installDomMocks(opts: DomMockOptions = {}): () => void {
  const width = opts.viewportWidth ?? DEFAULT_VIEWPORT_WIDTH_PX;
  const height = opts.viewportHeight ?? DEFAULT_VIEWPORT_HEIGHT_PX;
  const reducedMotion = opts.reducedMotion ?? false;
  const darkScheme = opts.darkScheme ?? false;

  const origResizeObserver = globalThis.ResizeObserver;
  const origMatchMedia = globalThis.matchMedia;

  class DeterministicResizeObserver implements ResizeObserver {
    private readonly cb: ResizeObserverCallback;
    constructor(cb: ResizeObserverCallback) {
      this.cb = cb;
    }
    observe(target: Element): void {
      const rect = {
        x: 0,
        y: 0,
        top: 0,
        left: 0,
        bottom: height,
        right: width,
        width,
        height,
        toJSON: () => ({}),
      } as DOMRectReadOnly;
      const entry = {
        target,
        contentRect: rect,
        borderBoxSize: [{ inlineSize: width, blockSize: height }],
        contentBoxSize: [{ inlineSize: width, blockSize: height }],
        devicePixelContentBoxSize: [{ inlineSize: width, blockSize: height }],
      } as unknown as ResizeObserverEntry;
      this.cb([entry], this);
    }
    unobserve(): void {}
    disconnect(): void {}
  }

  const mockMatchMedia = (query: string): MediaQueryList => {
    const matches =
      query.includes('prefers-reduced-motion: reduce')
        ? reducedMotion
        : query.includes('prefers-color-scheme: dark')
          ? darkScheme
          : false;
    return {
      matches,
      media: query,
      onchange: null,
      addListener: () => {},
      removeListener: () => {},
      addEventListener: () => {},
      removeEventListener: () => {},
      dispatchEvent: () => false,
    };
  };

  globalThis.ResizeObserver = DeterministicResizeObserver;
  globalThis.matchMedia = mockMatchMedia;

  return () => {
    globalThis.ResizeObserver = origResizeObserver;
    globalThis.matchMedia = origMatchMedia;
  };
}
