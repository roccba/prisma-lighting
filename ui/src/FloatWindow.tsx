import { useEffect, useRef, useState, type ReactNode } from "react";

// A floating tool window in the professional consoles style (Caja de Fader, Tabla de color, …):
// FIXED size (no resize), non-blocking (no backdrop — the rest of the console stays
// usable), opens where it was asked for (the click point) kept fully on screen, and
// moves by dragging its title bar. Escape or × closes it.

interface FloatWindowProps {
  title: string;
  /** Open at this point (usually the click that opened it); centred if omitted. */
  at?: { x: number; y: number };
  className?: string;
  onClose: () => void;
  children: ReactNode;
}

export function FloatWindow({ title, at, className, onClose, children }: FloatWindowProps) {
  const [pos, setPos] = useState<{ x: number; y: number } | null>(null);
  const boxRef = useRef<HTMLDivElement>(null);

  useEffect(() => {
    const el = boxRef.current;
    if (!el || pos) return;
    const w = el.offsetWidth;
    const h = el.offsetHeight;
    const x = at ? at.x : (window.innerWidth - w) / 2;
    const y = at ? at.y : (window.innerHeight - h) / 2;
    setPos({
      x: Math.max(4, Math.min(x, window.innerWidth - w - 4)),
      y: Math.max(4, Math.min(y, window.innerHeight - h - 4)),
    });
  }, [pos, at]);

  useEffect(() => {
    const onKey = (e: KeyboardEvent) => {
      // Escape belongs to the colour selector while it is held open.
      if (e.key === "Escape" && !document.body.classList.contains("is-picking")) onClose();
    };
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
  }, [onClose]);

  const startDrag = (e: React.PointerEvent<HTMLDivElement>) => {
    if (e.button !== 0 || !pos) return;
    const head = e.currentTarget;
    const dx = e.clientX - pos.x;
    const dy = e.clientY - pos.y;
    head.setPointerCapture(e.pointerId);
    const move = (ev: PointerEvent) => {
      const el = boxRef.current;
      const w = el?.offsetWidth ?? 0;
      const h = el?.offsetHeight ?? 0;
      setPos({
        x: Math.min(Math.max(0, ev.clientX - dx), window.innerWidth - w),
        y: Math.min(Math.max(0, ev.clientY - dy), window.innerHeight - h),
      });
    };
    const up = () => {
      head.removeEventListener("pointermove", move);
      head.removeEventListener("pointerup", up);
    };
    head.addEventListener("pointermove", move);
    head.addEventListener("pointerup", up);
  };

  return (
    <div
      ref={boxRef}
      className={`pr-float${className ? ` ${className}` : ""}`}
      role="dialog"
      aria-label={title}
      style={pos ? { left: pos.x, top: pos.y } : { visibility: "hidden" }}
    >
      <div className="pr-float-head" onPointerDown={startDrag}>
        <span>{title}</span>
        <button type="button" className="pr-float-x" onPointerDown={(e) => e.stopPropagation()} onClick={onClose} aria-label="Cerrar">
          ×
        </button>
      </div>
      {children}
    </div>
  );
}
