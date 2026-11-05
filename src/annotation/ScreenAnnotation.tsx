import { convertFileSrc, invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";
import { ArrowRight, Check, Trash2 } from "lucide-react";
import React, { useCallback, useEffect, useRef, useState } from "react";
import { useTranslation } from "react-i18next";
import { events } from "@/bindings";
import type { StreamTextEvent } from "@/bindings";

interface ScreenAnnotationSnapshot {
  session_id: number;
  path: string;
  width: number;
  height: number;
  point_count: number;
}

interface Point {
  x: number;
  y: number;
}

interface AnnotationPoint {
  number: number;
  position?: Point;
}

function markerPosition(
  point: AnnotationPoint,
  width: number,
  height: number,
  scale: number,
): Point {
  const margin = 18 * scale;
  const rows = Math.max(1, Math.floor((height - margin * 2) / (36 * scale)));
  const position = point.position ?? {
    x: margin + Math.floor((point.number - 1) / rows) * 36 * scale,
    y: margin + ((point.number - 1) % rows) * 36 * scale,
  };
  return {
    x: Math.max(margin, Math.min(width - margin, position.x)),
    y: Math.max(margin, Math.min(height - margin, position.y)),
  };
}

const ScreenAnnotation: React.FC = () => {
  const { t } = useTranslation();
  const canvasRef = useRef<HTMLCanvasElement>(null);
  const snapshotImageRef = useRef<HTMLImageElement>();
  const snapshotReadyRef = useRef<Promise<void>>();
  const drawingRef = useRef(false);
  const lastPointRef = useRef<Point>();
  const sessionIdRef = useRef<number>();
  const pointsRef = useRef<AnnotationPoint[]>([{ number: 1 }]);
  const pointChangeRef = useRef<Promise<void>>();
  const finalizingRef = useRef(false);
  const [points, setPoints] = useState(pointsRef.current);
  const [imageSize, setImageSize] = useState({ width: 1, height: 1 });
  const [busy, setBusy] = useState(false);
  const [finishing, setFinishing] = useState(false);
  const [actionFailed, setActionFailed] = useState(false);
  const [color, setColor] = useState("#ff2d55");
  const [brushSize, setBrushSize] = useState(7);
  const [streamText, setStreamText] = useState<StreamTextEvent>({
    committed: "",
    tentative: "",
  });

  const updatePoints = useCallback((updated: AnnotationPoint[]) => {
    pointsRef.current = updated;
    setPoints(updated);
  }, []);

  const syncPointCount = useCallback(
    (count: number) => {
      const current = pointsRef.current;
      if (count <= current.length) return;
      drawingRef.current = false;
      lastPointRef.current = undefined;
      updatePoints([
        ...current,
        ...Array.from({ length: count - current.length }, (_, index) => ({
          number: current.length + index + 1,
        })),
      ]);
    },
    [updatePoints],
  );

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
    const sessionId = sessionIdRef.current;
    if (sessionId === undefined) return;
    finalizingRef.current = true;
    setFinishing(true);
    await snapshotReadyRef.current;
    await pointChangeRef.current;
    if (sessionIdRef.current !== sessionId) return;
    const canvas = canvasRef.current;
    if (!canvas) throw new Error("The annotation canvas is unavailable");
    const output = document.createElement("canvas");
    output.width = canvas.width;
    output.height = canvas.height;
    const context = output.getContext("2d");
    if (!context) throw new Error("Cannot render the numbered screenshot");
    context.drawImage(canvas, 0, 0);
    const scale = canvas.width / canvas.getBoundingClientRect().width;
    for (const point of pointsRef.current) {
      const position = markerPosition(
        point,
        output.width,
        output.height,
        scale,
      );
      context.beginPath();
      context.arc(position.x, position.y, 14 * scale, 0, Math.PI * 2);
      context.fillStyle = "#111";
      context.fill();
      context.lineWidth = 2 * scale;
      context.strokeStyle = "#fff";
      context.stroke();
      context.font = `bold ${14 * scale}px sans-serif`;
      context.textAlign = "center";
      context.textBaseline = "middle";
      context.fillStyle = "#fff";
      context.fillText(String(point.number), position.x, position.y);
    }
    await invoke("submit_screen_annotation", {
      sessionId,
      pngDataUrl: output.toDataURL("image/png"),
      pointCount: pointsRef.current.length,
    });
  }, []);

  const advancePoint = useCallback(() => {
    const sessionId = sessionIdRef.current;
    if (
      sessionId === undefined ||
      pointChangeRef.current ||
      finalizingRef.current
    )
      return;
    setBusy(true);
    setActionFailed(false);
    drawingRef.current = false;
    lastPointRef.current = undefined;
    const operation = (async () => {
      try {
        const number = await invoke<number>("advance_screen_annotation", {
          sessionId,
          pointNumber: pointsRef.current.length,
        });
        if (sessionIdRef.current === sessionId) {
          syncPointCount(number);
        }
      } catch (error) {
        console.error("Failed to advance the annotation point:", error);
        if (sessionIdRef.current === sessionId && !finalizingRef.current)
          setActionFailed(true);
      } finally {
        if (sessionIdRef.current === sessionId) {
          pointChangeRef.current = undefined;
          setBusy(false);
        }
      }
    })();
    pointChangeRef.current = operation;
  }, [syncPointCount]);

  const complete = async () => {
    setActionFailed(false);
    try {
      await invoke("complete_screen_annotation");
    } catch (error) {
      console.error("Failed to complete the annotation:", error);
      setActionFailed(true);
    }
  };

  useEffect(() => {
    let disposed = false;
    let unlistenFinish: (() => void) | undefined;
    let unlistenReset: (() => void) | undefined;
    let unlistenStream: (() => void) | undefined;
    let unlistenNext: (() => void) | undefined;

    const loadSnapshot = (snapshot: ScreenAnnotationSnapshot) => {
      if (disposed) return;
      sessionIdRef.current = snapshot.session_id;
      finalizingRef.current = false;
      pointChangeRef.current = undefined;
      drawingRef.current = false;
      lastPointRef.current = undefined;
      updatePoints(
        Array.from({ length: snapshot.point_count }, (_, index) => ({
          number: index + 1,
        })),
      );
      setFinishing(false);
      setBusy(false);
      setActionFailed(false);

      const image = new Image();
      image.crossOrigin = "anonymous";
      const loaded = new Promise<void>((resolve, reject) => {
        image.onload = () => resolve();
        image.onerror = () =>
          reject(new Error("Cannot load the captured screenshot"));
      });
      snapshotReadyRef.current = loaded.then(async () => {
        if (disposed || sessionIdRef.current !== snapshot.session_id) return;
        const canvas = canvasRef.current;
        if (!canvas) throw new Error("The annotation canvas is unavailable");
        canvas.width = snapshot.width;
        canvas.height = snapshot.height;
        setImageSize({ width: snapshot.width, height: snapshot.height });
        snapshotImageRef.current = image;
        drawSnapshot();
        const latest = await invoke<ScreenAnnotationSnapshot>(
          "get_screen_annotation_snapshot",
        );
        if (disposed || sessionIdRef.current !== latest.session_id) return;
        syncPointCount(latest.point_count);
      });
      void snapshotReadyRef.current
        .then(async () => {
          if (
            disposed ||
            sessionIdRef.current !== snapshot.session_id ||
            finalizingRef.current
          )
            return;
          await invoke("show_screen_annotation", {
            sessionId: snapshot.session_id,
          });
        })
        .catch((error) => {
          console.error("Failed to prepare the annotation screenshot:", error);
          if (!disposed && sessionIdRef.current === snapshot.session_id) {
            setActionFailed(true);
          }
        });
      image.src = convertFileSrc(snapshot.path);
    };

    const initialize = async () => {
      [unlistenFinish, unlistenReset, unlistenStream, unlistenNext] =
        await Promise.all([
          listen("finish-screen-annotation", () => {
            const sessionId = sessionIdRef.current;
            void finalize().catch((error) => {
              console.error("Failed to submit the numbered screenshot:", error);
              if (sessionIdRef.current === sessionId) setActionFailed(true);
            });
          }),
          listen<ScreenAnnotationSnapshot>(
            "reset-screen-annotation",
            (event) => {
              setStreamText({ committed: "", tentative: "" });
              loadSnapshot(event.payload);
            },
          ),
          events.streamTextEvent.listen((event) =>
            setStreamText(event.payload),
          ),
          listen<ScreenAnnotationSnapshot>(
            "annotation-point-changed",
            (event) => {
              if (sessionIdRef.current === event.payload.session_id) {
                syncPointCount(event.payload.point_count);
              }
            },
          ),
        ]);
      if (disposed) {
        unlistenFinish();
        unlistenReset();
        unlistenStream();
        unlistenNext();
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
      unlistenNext?.();
    };
  }, [drawSnapshot, finalize, updatePoints, syncPointCount]);

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
    if (pointChangeRef.current || finalizingRef.current) return;
    const point = pointFromEvent(event);
    if (!point) return;
    event.currentTarget.setPointerCapture(event.pointerId);
    drawingRef.current = true;
    lastPointRef.current = point;
    const current = pointsRef.current[pointsRef.current.length - 1];
    if (current && !current.position) {
      updatePoints([
        ...pointsRef.current.slice(0, -1),
        { ...current, position: point },
      ]);
    }
  };

  const continueStroke = (event: React.PointerEvent<HTMLCanvasElement>) => {
    if (!drawingRef.current || finalizingRef.current) return;
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
      <div className="annotation-markers" aria-live="polite">
        {points.map((point) => {
          const scale =
            imageSize.width /
            (canvasRef.current?.getBoundingClientRect().width ||
              imageSize.width);
          const position = markerPosition(
            point,
            imageSize.width,
            imageSize.height,
            scale,
          );
          return (
            <span
              key={point.number}
              className="annotation-marker"
              aria-label={t("screenAnnotation.point", { number: point.number })}
              style={{
                left: `${(position.x / imageSize.width) * 100}%`,
                top: `${(position.y / imageSize.height) * 100}%`,
              }}
            >
              {point.number}
            </span>
          );
        })}
      </div>
      <div className="annotation-hud">
        <div className="annotation-toolbar">
          <span className="annotation-recording-dot" />
          <input
            className="annotation-color"
            type="color"
            aria-label={t("screenAnnotation.inkColor")}
            value={color}
            onChange={(event) => setColor(event.target.value)}
          />
          <input
            className="annotation-size"
            type="range"
            aria-label={t("screenAnnotation.strokeWidth")}
            min="2"
            max="24"
            value={brushSize}
            onChange={(event) => setBrushSize(Number(event.target.value))}
          />
          <button
            className="annotation-button"
            title={t("screenAnnotation.clearInk")}
            aria-label={t("screenAnnotation.clearInk")}
            onClick={drawSnapshot}
            disabled={busy || finishing}
          >
            <Trash2 aria-hidden="true" />
          </button>
          <button
            className="annotation-button annotation-next"
            title={t("screenAnnotation.nextPointShortcut")}
            onClick={advancePoint}
            disabled={busy || finishing || !points[points.length - 1]?.position}
          >
            {t("screenAnnotation.nextPoint")}
            <ArrowRight aria-hidden="true" />
          </button>
          <button
            className="annotation-button annotation-done"
            title={t("settings.history.save")}
            aria-label={t("settings.history.save")}
            onClick={() => void complete()}
            disabled={busy || finishing}
          >
            <Check aria-hidden="true" />
          </button>
        </div>
        {actionFailed && (
          <div className="annotation-transcript" role="alert">
            {t("screenAnnotation.actionFailed")}
          </div>
        )}
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
