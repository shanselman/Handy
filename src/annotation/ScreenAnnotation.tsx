import { convertFileSrc, invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";
import { Check, Trash2 } from "lucide-react";
import React, { useCallback, useEffect, useRef, useState } from "react";
import { useTranslation } from "react-i18next";
import { events } from "@/bindings";
import type { StreamTextEvent } from "@/bindings";

interface ScreenAnnotationSnapshot {
  session_id: number;
  path: string;
  width: number;
  height: number;
}

interface Point {
  x: number;
  y: number;
}

const ScreenAnnotation: React.FC = () => {
  const { t } = useTranslation();
  const canvasRef = useRef<HTMLCanvasElement>(null);
  const snapshotImageRef = useRef<HTMLImageElement>();
  const drawingRef = useRef(false);
  const lastPointRef = useRef<Point>();
  const sessionIdRef = useRef<number>();
  const [color, setColor] = useState("#ff2d55");
  const [brushSize, setBrushSize] = useState(7);
  const [streamText, setStreamText] = useState<StreamTextEvent>({
    committed: "",
    tentative: "",
  });

  const drawSnapshot = useCallback(() => {
    const canvas = canvasRef.current;
    const image = snapshotImageRef.current;
    if (!canvas || !image) return;
    const context = canvas.getContext("2d");
    if (!context) return;
    context.clearRect(0, 0, canvas.width, canvas.height);
    context.drawImage(image, 0, 0, canvas.width, canvas.height);
  }, []);

  const finalize = useCallback(async () => {
    const canvas = canvasRef.current;
    const sessionId = sessionIdRef.current;
    if (!canvas || sessionId === undefined) return;
    const pngDataUrl = canvas.toDataURL("image/png");
    await invoke("submit_screen_annotation", { sessionId, pngDataUrl });
  }, []);

  useEffect(() => {
    let disposed = false;
    let unlistenFinish: (() => void) | undefined;
    let unlistenReset: (() => void) | undefined;
    let unlistenStream: (() => void) | undefined;

    const loadSnapshot = (snapshot: ScreenAnnotationSnapshot) => {
      if (disposed) return;
      sessionIdRef.current = snapshot.session_id;

      const image = new Image();
      image.crossOrigin = "anonymous";
      image.onload = async () => {
        if (disposed || sessionIdRef.current !== snapshot.session_id) return;
        const canvas = canvasRef.current;
        if (!canvas) return;
        canvas.width = snapshot.width;
        canvas.height = snapshot.height;
        snapshotImageRef.current = image;
        drawSnapshot();
        await invoke("show_screen_annotation", {
          sessionId: snapshot.session_id,
        });
      };
      image.src = convertFileSrc(snapshot.path);
    };

    const initialize = async () => {
      [unlistenFinish, unlistenReset, unlistenStream] = await Promise.all([
        listen("finish-screen-annotation", finalize),
        listen<ScreenAnnotationSnapshot>("reset-screen-annotation", (event) => {
          setStreamText({ committed: "", tentative: "" });
          loadSnapshot(event.payload);
        }),
        events.streamTextEvent.listen((event) => setStreamText(event.payload)),
      ]);
      if (disposed) {
        unlistenFinish();
        unlistenReset();
        unlistenStream();
        return;
      }
      const snapshot = await invoke<ScreenAnnotationSnapshot>(
        "get_screen_annotation_snapshot",
      ).catch(() => undefined);
      if (snapshot) loadSnapshot(snapshot);
    };

    void initialize();
    return () => {
      disposed = true;
      unlistenFinish?.();
      unlistenReset?.();
      unlistenStream?.();
    };
  }, [drawSnapshot, finalize]);

  const pointFromEvent = (event: React.PointerEvent<HTMLCanvasElement>) => {
    const canvas = canvasRef.current;
    if (!canvas) return undefined;
    const rect = canvas.getBoundingClientRect();
    return {
      x: (event.clientX - rect.left) * (canvas.width / rect.width),
      y: (event.clientY - rect.top) * (canvas.height / rect.height),
    };
  };

  const beginStroke = (event: React.PointerEvent<HTMLCanvasElement>) => {
    const point = pointFromEvent(event);
    if (!point) return;
    event.currentTarget.setPointerCapture(event.pointerId);
    drawingRef.current = true;
    lastPointRef.current = point;
  };

  const continueStroke = (event: React.PointerEvent<HTMLCanvasElement>) => {
    if (!drawingRef.current) return;
    const canvas = canvasRef.current;
    const point = pointFromEvent(event);
    const previous = lastPointRef.current;
    if (!canvas || !point || !previous) return;

    const context = canvas.getContext("2d");
    if (!context) return;
    const scale = canvas.width / canvas.getBoundingClientRect().width;
    context.strokeStyle = color;
    context.lineWidth = brushSize * scale;
    context.lineCap = "round";
    context.lineJoin = "round";
    context.beginPath();
    context.moveTo(previous.x, previous.y);
    context.lineTo(point.x, point.y);
    context.stroke();
    lastPointRef.current = point;
  };

  const endStroke = () => {
    drawingRef.current = false;
    lastPointRef.current = undefined;
  };

  return (
    <main className="annotation-stage">
      <canvas
        ref={canvasRef}
        className="annotation-canvas"
        onPointerDown={beginStroke}
        onPointerMove={continueStroke}
        onPointerUp={endStroke}
        onPointerCancel={endStroke}
      />
      <div className="annotation-hud">
        <div className="annotation-toolbar">
          <span className="annotation-recording-dot" />
          <input
            className="annotation-color"
            type="color"
            value={color}
            onChange={(event) => setColor(event.target.value)}
          />
          <input
            className="annotation-size"
            type="range"
            min="2"
            max="24"
            value={brushSize}
            onChange={(event) => setBrushSize(Number(event.target.value))}
          />
          <button
            className="annotation-button"
            title={t("common.clear")}
            aria-label={t("common.clear")}
            onClick={drawSnapshot}
          >
            <Trash2 aria-hidden="true" />
          </button>
          <button
            className="annotation-button annotation-done"
            title={t("settings.history.save")}
            aria-label={t("settings.history.save")}
            onClick={() => invoke("complete_screen_annotation")}
          >
            <Check aria-hidden="true" />
          </button>
        </div>
        {(streamText.committed || streamText.tentative) && (
          <div className="annotation-transcript" aria-live="polite">
            <span>{streamText.committed}</span>
            {streamText.committed && streamText.tentative ? " " : ""}
            <span className="annotation-tentative">{streamText.tentative}</span>
          </div>
        )}
      </div>
    </main>
  );
};

export default ScreenAnnotation;
