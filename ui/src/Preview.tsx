import { useEffect, useRef, useState } from "react";
import { previewPort } from "./api";

// S10: the live preview finally rendered INSIDE the app — closing the M6 debt, which
// shipped the PreviewSink + wire format + a standalone HTML viewer but no in-webview
// component. This connects to the engine's preview WebSocket (§M6) and draws the rig
// the way the standalone viewer does: each pixel at its real position, filled with the
// colour the engine rendered, ringed in its zone's colour. In M7 there is no show
// content yet, so fills are black (IdleContent) and what you see is the engine's live
// view of the rig geometry + zones; the fills light up once M8 runs real content.

interface Descriptor {
  generation: number;
  pixels: { x: number; y: number }[];
  zones: { id: string; name: string; members: number[] }[];
}

const MAGIC = [0x50, 0x50, 0x56, 0x31]; // "PPV1"

function zoneColor(i: number, n: number): string {
  return `hsl(${((i * 360) / Math.max(1, n)) % 360} 85% 60%)`;
}

export function Preview() {
  const canvasRef = useRef<HTMLCanvasElement>(null);
  const descriptorRef = useRef<Descriptor | null>(null);
  const pixelZoneRef = useRef<number[]>([]);
  const colorsRef = useRef<Uint8Array | null>(null);
  const [status, setStatus] = useState<"off" | "connecting" | "on">("off");
  const [fps, setFps] = useState(0);

  useEffect(() => {
    let ws: WebSocket | null = null;
    let reconnect: number | undefined;
    let closed = false;
    const frameTimes: number[] = [];

    const draw = () => {
      const canvas = canvasRef.current;
      const d = descriptorRef.current;
      const colors = colorsRef.current;
      if (!canvas) return;
      const dpr = window.devicePixelRatio || 1;
      const w = Math.max(1, Math.round(canvas.clientWidth * dpr));
      const h = Math.max(1, Math.round(canvas.clientHeight * dpr));
      if (canvas.width !== w || canvas.height !== h) { canvas.width = w; canvas.height = h; }
      const ctx = canvas.getContext("2d")!;
      ctx.fillStyle = "#050506";
      ctx.fillRect(0, 0, w, h);
      if (!d || !colors || d.pixels.length === 0) return;

      let minX = Infinity, maxX = -Infinity, minY = Infinity, maxY = -Infinity;
      for (const p of d.pixels) {
        minX = Math.min(minX, p.x); maxX = Math.max(maxX, p.x);
        minY = Math.min(minY, p.y); maxY = Math.max(maxY, p.y);
      }
      const spanX = Math.max(1e-3, maxX - minX), spanY = Math.max(1e-3, maxY - minY);
      const pad = 16 * dpr;
      const s = Math.min((w - 2 * pad) / spanX, (h - 2 * pad) / spanY);
      const r = Math.max(1.5, Math.min(s * 0.42, 14));
      const ox = (w - s * spanX) / 2 - minX * s;
      const oy = (h - s * spanY) / 2 - minY * s;
      const n = d.zones.length;

      // SQUARE dots via `fillRect` (a pixel is square; uniform with the main-window
      // previews). Critically, `arc`+`stroke` PER PIXEL tessellates two paths per pixel —
      // over the whole rig (thousands of pixels × many zones) that saturated the UI thread
      // and made the whole editor sluggish (slow to open, delete/edit felt dead). The
      // per-pixel zone ring is an identification aid that only helps at low density, so it
      // is drawn only for small rigs; at scale the fill colour alone carries the content.
      const many = d.pixels.length > 400;
      const dd = 2 * r;
      for (let i = 0; i < d.pixels.length; i++) {
        const cx = ox + d.pixels[i].x * s, cy = oy + d.pixels[i].y * s;
        ctx.fillStyle = `rgb(${colors[i * 3]},${colors[i * 3 + 1]},${colors[i * 3 + 2]})`;
        ctx.fillRect(cx - r, cy - r, dd, dd);
        if (!many) {
          const zs = pixelZoneRef.current[i];
          if (zs >= 0) {
            ctx.lineWidth = Math.max(1, r * 0.28);
            ctx.strokeStyle = zoneColor(zs, n);
            ctx.strokeRect(cx - r, cy - r, dd, dd);
          }
        }
      }
    };

    const onDescriptor = (text: string) => {
      try {
        const d: Descriptor = JSON.parse(text);
        descriptorRef.current = d;
        const pz = new Array(d.pixels.length).fill(-1);
        d.zones.forEach((z, slot) => z.members.forEach((p) => { if (pz[p] < 0) pz[p] = slot; }));
        pixelZoneRef.current = pz;
      } catch { /* ignore a malformed descriptor */ }
    };

    const onFrame = (buf: ArrayBuffer) => {
      const dv = new DataView(buf);
      for (let i = 0; i < 4; i++) if (dv.getUint8(i) !== MAGIC[i]) return;
      const pixelCount = dv.getUint32(24, true);
      const zoneCount = dv.getUint16(28, true);
      const colors = new Uint8Array(buf, 32 + zoneCount * 4, pixelCount * 3);
      colorsRef.current = colors;

      const now = performance.now();
      frameTimes.push(now);
      while (frameTimes.length > 30) frameTimes.shift();
      if (frameTimes.length > 1) {
        const span = (frameTimes[frameTimes.length - 1] - frameTimes[0]) / 1000;
        if (span > 0) setFps(Math.round((frameTimes.length - 1) / span));
      }
      draw();
    };

    const connect = async () => {
      if (closed) return;
      setStatus("connecting");
      let port: number;
      try { port = await previewPort(); } catch { scheduleReconnect(); return; }
      if (closed) return;
      ws = new WebSocket(`ws://127.0.0.1:${port}`);
      ws.binaryType = "arraybuffer";
      ws.onopen = () => setStatus("on");
      ws.onmessage = (ev) => {
        if (typeof ev.data === "string") onDescriptor(ev.data);
        else onFrame(ev.data as ArrayBuffer);
      };
      ws.onclose = () => { setStatus("off"); scheduleReconnect(); };
      ws.onerror = () => { try { ws?.close(); } catch { /* already closing */ } };
    };

    const scheduleReconnect = () => {
      if (closed) return;
      window.clearTimeout(reconnect);
      reconnect = window.setTimeout(connect, 1000);
    };

    connect();
    const onResize = () => draw();
    window.addEventListener("resize", onResize);
    return () => {
      closed = true;
      window.clearTimeout(reconnect);
      window.removeEventListener("resize", onResize);
      try { ws?.close(); } catch { /* already closing */ }
    };
  }, []);

  return (
    <div className="preview">
      <div className="preview-head">
        <span className="preview-title">Live (engine)</span>
        <span className={`preview-status ${status}`}>{status}</span>
        {status === "on" && <span className="preview-fps">{fps} fps</span>}
      </div>
      <canvas ref={canvasRef} className="preview-canvas-live" />
    </div>
  );
}
