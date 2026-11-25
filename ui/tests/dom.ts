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
  const origOffsetParent = Object.getOwnPropertyDescriptor(HTMLElement.prototype, 'offsetParent');

  // jsdom always reports offsetParent === null; virtua relies on offsetParent to measure mounted items
  // and clear initial `visibility: hidden` once an item enters the viewport.
  Object.defineProperty(HTMLElement.prototype, 'offsetParent', {
    configurable: true,
    get(this: HTMLElement) {
      return this.isConnected ? document.body : null;
    },
  });

  const DEFAULT_ITEM_HEIGHT_PX = 80;

  class DeterministicResizeObserver implements ResizeObserver {
    private readonly cb: ResizeObserverCallback;
    private readonly observed = new Set<Element>();
    private disconnected = false;
    constructor(cb: ResizeObserverCallback) {
      this.cb = cb;
    }
    observe(target: Element): void {
      this.observed.add(target);
      const isVirtualItem = target instanceof HTMLElement && target.style.position === 'absolute';
      const measuredHeight = isVirtualItem ? DEFAULT_ITEM_HEIGHT_PX : height;
      const rect = {
        x: 0,
        y: 0,
        top: 0,
        left: 0,
        bottom: measuredHeight,
        right: width,
        width,
        height: measuredHeight,
        toJSON: () => ({}),
      } as DOMRectReadOnly;
      const entry = {
        target,
        contentRect: rect,
        borderBoxSize: [{ inlineSize: width, blockSize: measuredHeight }],
        contentBoxSize: [{ inlineSize: width, blockSize: measuredHeight }],
        devicePixelContentBoxSize: [{ inlineSize: width, blockSize: measuredHeight }],
      } as unknown as ResizeObserverEntry;
      // Deliver asynchronously on microtask like real browsers so callers finish setup after observe().
      queueMicrotask(() => {
        if (!this.disconnected && this.observed.has(target)) {
          this.cb([entry], this);
        }
      });
    }
    unobserve(target: Element): void {
      this.observed.delete(target);
    }
    disconnect(): void {
      this.disconnected = true;
      this.observed.clear();
    }
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
    if (origOffsetParent) {
      Object.defineProperty(HTMLElement.prototype, 'offsetParent', origOffsetParent);
    } else {
      delete (HTMLElement.prototype as { offsetParent?: unknown }).offsetParent;
    }
  };
}
