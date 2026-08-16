import { createContext, useContext, useEffect, useRef, useState, type ReactNode } from "react";
import { previewPort } from "./api";

// A single shared connection to the engine's live-preview WebSocket (§M6). The main
// window has several consumers of the same stream — every zone card's mini-preview,
// and later the large preview — so they must NOT each open a socket. This provider
// owns one connection, keeps the latest descriptor + colour buffer in refs, and
// notifies subscribers once per frame; each consumer draws its own slice (a zone's
// member pixels) from the shared buffers. This is the "mini-preview = a filter over
// the existing M6 stream" design.

export interface PreviewDescriptor {
  generation: number;
  pixels: { x: number; y: number }[];
  zones: { id: string; name: string; members: number[] }[];
}

interface PreviewStream {
  descriptorRef: React.MutableRefObject<PreviewDescriptor | null>;
  colorsRef: React.MutableRefObject<Uint8Array | null>;
  /** Register a per-frame callback; returns an unsubscribe. */
  subscribe: (fn: () => void) => () => void;
  status: "off" | "connecting" | "on";
}

const MAGIC = [0x50, 0x50, 0x56, 0x31]; // "PPV1"

const Ctx = createContext<PreviewStream | null>(null);

export function PreviewStreamProvider({ children }: { children: ReactNode }) {
  const descriptorRef = useRef<PreviewDescriptor | null>(null);
  const colorsRef = useRef<Uint8Array | null>(null);
  const subscribers = useRef<Set<() => void>>(new Set());
  const [status, setStatus] = useState<"off" | "connecting" | "on">("off");

  useEffect(() => {
    let ws: WebSocket | null = null;
    let reconnect: number | undefined;
    let closed = false;

    const notify = () => {
      for (const fn of subscribers.current) fn();
    };

    const onFrame = (buf: ArrayBuffer) => {
      const dv = new DataView(buf);
      for (let i = 0; i < 4; i++) if (dv.getUint8(i) !== MAGIC[i]) return;
      const pixelCount = dv.getUint32(24, true);
      const zoneCount = dv.getUint16(28, true);
      colorsRef.current = new Uint8Array(buf, 32 + zoneCount * 4, pixelCount * 3);
      notify();
    };

    const onDescriptor = (text: string) => {
      try {
        descriptorRef.current = JSON.parse(text) as PreviewDescriptor;
        notify();
      } catch {
        /* ignore a malformed descriptor */
      }
    };

    const connect = async () => {
      if (closed) return;
      setStatus("connecting");
      let port: number;
      try {
        port = await previewPort();
      } catch {
        scheduleReconnect();
        return;
      }
      if (closed) return;
      ws = new WebSocket(`ws://127.0.0.1:${port}`);
      ws.binaryType = "arraybuffer";
      ws.onopen = () => setStatus("on");
      ws.onmessage = (ev) => {
        if (typeof ev.data === "string") onDescriptor(ev.data);
        else onFrame(ev.data as ArrayBuffer);
      };
      ws.onclose = () => {
        setStatus("off");
        scheduleReconnect();
      };
      ws.onerror = () => {
        try {
          ws?.close();
        } catch {
          /* already closing */
        }
      };
    };

    const scheduleReconnect = () => {
      if (closed) return;
      window.clearTimeout(reconnect);
      reconnect = window.setTimeout(connect, 1000);
    };

    connect();
    return () => {
      closed = true;
      window.clearTimeout(reconnect);
      try {
        ws?.close();
      } catch {
        /* already closing */
      }
    };
  }, []);

  const subscribe = (fn: () => void) => {
    subscribers.current.add(fn);
    return () => {
      subscribers.current.delete(fn);
    };
  };

  return (
    <Ctx.Provider value={{ descriptorRef, colorsRef, subscribe, status }}>{children}</Ctx.Provider>
  );
}

export function usePreviewStream(): PreviewStream {
  const ctx = useContext(Ctx);
  if (!ctx) throw new Error("usePreviewStream must be used within a PreviewStreamProvider");
  return ctx;
}
