import { useEffect, useRef } from "react";
import { drawZone } from "./previewDraw";
import { usePreviewStream } from "./previewStream";

// The large live preview of the SELECTED zone (M8-SPEC §3.4): the zone's real fixture
// geometry with its effect running live. It consumes the same M6 stream the zone-row
// mini-previews use (`drawZone`, the shared path), just at a larger size with fuller
// dots — so it is the honest, big-enough view for judging an effect and for diagnosing
// the reported flicker (a stepping visible here on an animated effect points at
// content/temporal aliasing at a small mapping_size, not at the draw). Self-contained
// (§7): it assumes only a zone id and the stream provider above it.

interface LargePreviewProps {
  zoneId: string;
}

export function LargePreview({ zoneId }: LargePreviewProps) {
  const canvasRef = useRef<HTMLCanvasElement>(null);
  const stream = usePreviewStream();

  useEffect(() => {
    const draw = () => {
      const canvas = canvasRef.current;
      if (!canvas) return;
      drawZone(canvas, stream.descriptorRef.current, stream.colorsRef.current, zoneId, {
        dotScale: 0.5,
        maxDotCss: 14,
        padCss: 16,
      });
    };
    const unsub = stream.subscribe(draw);
    draw();
    return unsub;
  }, [zoneId, stream]);

  return <canvas ref={canvasRef} className="pr-largepreview-canvas" />;
}
