import { useMemo } from 'react';
import { encode as encodeQr } from 'uqr';

export type QrCodeProps = {
  value: string;
  label: string;
  size?: number;
};

/** Offline SVG QR code rendered via uqr's boolean matrix as a pure JSX <path> element. */
export function QrCode({ value, label, size = 96 }: QrCodeProps) {
  const { viewSize, pathD } = useMemo(() => {
    const matrix = encodeQr(value, { border: 2 });
    const parts: string[] = [];
    for (let y = 0; y < matrix.size; y++) {
      for (let x = 0; x < matrix.size; x++) {
        if (matrix.data[y]?.[x]) {
          parts.push(`M${x} ${y}h1v1h-1z`);
        }
      }
    }
    return { viewSize: matrix.size, pathD: parts.join('') };
  }, [value]);

  return (
    <div className="qr">
      <svg
        width={size}
        height={size}
        viewBox={`0 0 ${viewSize} ${viewSize}`}
        role="img"
        aria-label={label}
        shapeRendering="crispEdges"
      >
        <rect width={viewSize} height={viewSize} fill="#ffffff" />
        <path d={pathD} fill="#0A0A0A" />
      </svg>
    </div>
  );
}
