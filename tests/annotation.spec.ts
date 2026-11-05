import { expect, test, type Page } from "@playwright/test";

declare global {
  interface Window {
    annotationTest: {
      deferNext: boolean;
      rejectAdvance: boolean;
      advances: { sessionId: number; pointNumber: number }[];
      submissions: {
        sessionId: number;
        pointCount: number;
        pngDataUrl: string;
      }[];
      resolveNext?: () => void;
      emit: (event: string, payload?: object) => Promise<void>;
    };
  }
}

test.beforeEach(async ({ page }) => {
  await page.route("**/src/annotation/index.html*", async (route) => {
    const response = await route.fetch();
    const setup = `<script type="module">
      import { mockIPC } from "/node_modules/@tauri-apps/api/mocks.js";
      import { emit } from "/node_modules/@tauri-apps/api/event.js";
      const state = window.annotationTest = {
        deferNext: false, rejectAdvance: false, advances: [], submissions: [],
        snapshot: { session_id: 42, path: "synthetic.png", width: 2560, height: 1440, point_count: 1 },
        emit: (name, payload) => {
          if (name === "reset-screen-annotation") state.snapshot = payload;
          if (name === "annotation-point-changed" && payload.session_id === state.snapshot.session_id)
            state.snapshot = { ...state.snapshot, point_count: payload.point_count };
          return emit(name, payload);
        },
      };
      const screenshot = document.createElement("canvas");
      screenshot.width = 2560;
      screenshot.height = 1440;
      const context = screenshot.getContext("2d");
      context.fillStyle = "#fff";
      context.fillRect(0, 0, screenshot.width, screenshot.height);
      mockIPC((command, args) => {
        switch (command) {
          case "get_app_settings": return { app_language: "en" };
          case "get_screen_annotation_snapshot":
            return state.snapshot;
          case "show_screen_annotation": return;
          case "advance_screen_annotation":
            state.advances.push(args);
            if (state.rejectAdvance) throw new Error("Point transition failed");
            if (state.deferNext) return new Promise(resolve => {
              state.resolveNext = () => resolve(args.pointNumber + 1);
            });
            return args.pointNumber + 1;
          case "complete_screen_annotation": return emit("finish-screen-annotation");
          case "submit_screen_annotation": state.submissions.push(args); return;
          default: throw new Error("Unexpected test command: " + command);
        }
      }, { shouldMockEvents: true });
      window.__TAURI_INTERNALS__.convertFileSrc = () =>
        new URLSearchParams(location.search).has("delayImage")
          ? "/annotation-test-image.png" : screenshot.toDataURL();
    </script>`;
    await route.fulfill({
      response,
      body: (await response.text()).replace("</head>", `${setup}</head>`),
    });
  });
  await page.goto("/src/annotation/index.html");
  await expect(page.locator("canvas")).toHaveJSProperty("width", 2560);
});

async function stroke(page: Page, x: number, y: number) {
  await page.mouse.move(x, y);
  await page.mouse.down();
  await page.mouse.move(x + 50, y + 30, { steps: 4 });
  await page.mouse.up();
}

test("groups strokes, preserves numbers when clearing, and exports scaled badges", async ({
  page,
}) => {
  const next = page.getByRole("button", { name: "Next point" });
  const markers = page.locator(".annotation-marker");
  await expect(next).toBeDisabled();
  await stroke(page, 220, 200);
  await stroke(page, 300, 220);
  await expect(markers).toHaveCount(1);
  await next.click();
  await expect(markers).toHaveCount(2);
  await stroke(page, 600, 250);
  await page.getByRole("button", { name: "Clear ink" }).click();
  await expect(markers).toHaveCount(2);
  await page.locator(".annotation-done").click();
  await expect
    .poll(() => page.evaluate(() => window.annotationTest.submissions.length))
    .toBe(1);
  const output = await page.evaluate(async () => {
    const submission = window.annotationTest.submissions[0];
    const image = new Image();
    image.src = submission.pngDataUrl;
    await image.decode();
    const canvas = document.createElement("canvas");
    canvas.width = image.width;
    canvas.height = image.height;
    const context = canvas.getContext("2d")!;
    context.drawImage(image, 0, 0);
    return {
      pointCount: submission.pointCount,
      firstBadge: Array.from(context.getImageData(454, 414, 1, 1).data),
      secondBadge: Array.from(context.getImageData(1214, 514, 1, 1).data),
      clearedInk: Array.from(context.getImageData(500, 436, 1, 1).data),
    };
  });
  expect(output).toEqual({
    pointCount: 2,
    firstBadge: [17, 17, 17, 255],
    secondBadge: [17, 17, 17, 255],
    clearedInk: [255, 255, 255, 255],
  });
});

test("finishing waits for a pending point boundary", async ({ page }) => {
  await stroke(page, 220, 200);
  await page.evaluate(() => {
    window.annotationTest.deferNext = true;
  });
  await page.getByRole("button", { name: "Next point" }).click();
  await page.evaluate(() =>
    window.annotationTest.emit("finish-screen-annotation"),
  );
  expect(
    await page.evaluate(() => window.annotationTest.submissions.length),
  ).toBe(0);
  await page.evaluate(() => window.annotationTest.resolveNext?.());
  await expect
    .poll(() =>
      page.evaluate(() => window.annotationTest.submissions[0]?.pointCount),
    )
    .toBe(2);
  expect(await page.evaluate(() => window.annotationTest.advances)).toEqual([
    { sessionId: 42, pointNumber: 1 },
  ]);
});

test("ignores an old session response and exposes point errors", async ({
  page,
}) => {
  await stroke(page, 220, 200);
  await page.evaluate(() => {
    window.annotationTest.deferNext = true;
  });
  const next = page.getByRole("button", { name: "Next point" });
  await next.click();
  await page.evaluate(() =>
    window.annotationTest.emit("finish-screen-annotation"),
  );
  await page.evaluate(() =>
    window.annotationTest.emit("reset-screen-annotation", {
      session_id: 43,
      path: "synthetic.png",
      width: 2560,
      height: 1440,
      point_count: 1,
    }),
  );
  await expect(next).toBeDisabled();
  await page.evaluate(() => {
    window.annotationTest.resolveNext?.();
    window.annotationTest.deferNext = false;
    window.annotationTest.rejectAdvance = true;
  });

  await expect(page.locator(".annotation-marker")).toHaveCount(1);
  expect(
    await page.evaluate(() => window.annotationTest.submissions.length),
  ).toBe(0);
  await stroke(page, 600, 250);
  await next.click();
  await expect(page.getByRole("alert")).toBeVisible();
  await expect(page.locator(".annotation-marker")).toHaveCount(1);
  await page.evaluate(() => {
    window.annotationTest.rejectAdvance = false;
  });
  await next.click();
  await expect(page.locator(".annotation-marker")).toHaveCount(2);
  expect(
    await page.evaluate(() => window.annotationTest.advances.slice(-1)),
  ).toEqual([{ sessionId: 43, pointNumber: 1 }]);
});

test("keyboard point updates do not duplicate boundaries or continue an old stroke", async ({
  page,
}) => {
  await page.mouse.move(220, 200);
  await page.mouse.down();
  await page.mouse.move(250, 220);
  await page.evaluate(async () => {
    const snapshot = {
      session_id: 42,
      path: "synthetic.png",
      width: 2560,
      height: 1440,
    };
    for (const point_count of [2, 3, 3]) {
      await window.annotationTest.emit("annotation-point-changed", {
        ...snapshot,
        point_count,
      });
    }
    await window.annotationTest.emit("annotation-point-changed", {
      ...snapshot,
      session_id: 41,
      point_count: 4,
    });
  });
  await expect(page.locator(".annotation-marker")).toHaveCount(3);
  await page.mouse.move(600, 250);
  await page.mouse.up();
  const pixel = await page
    .locator("canvas")
    .evaluate((canvas: HTMLCanvasElement) =>
      Array.from(canvas.getContext("2d")!.getImageData(1200, 500, 1, 1).data),
    );
  expect(pixel).toEqual([255, 255, 255, 255]);
  expect(await page.evaluate(() => window.annotationTest.advances)).toEqual([]);
  await stroke(page, 600, 250);
  await page.locator(".annotation-done").click();
  await expect
    .poll(() =>
      page.evaluate(() => window.annotationTest.submissions[0]?.pointCount),
    )
    .toBe(3);
});

test("a quick hold release waits for the captured image before exporting", async ({
  page,
}) => {
  let releaseImage = () => {};
  const imageReady = new Promise<void>((resolve) => {
    releaseImage = resolve;
  });
  await page.route("**/annotation-test-image.png", async (route) => {
    await imageReady;
    await route.fulfill({
      contentType: "image/svg+xml",
      body: '<svg xmlns="http://www.w3.org/2000/svg" width="2560" height="1440"><rect width="100%" height="100%" fill="white"/></svg>',
    });
  });
  await page.goto("/src/annotation/index.html?delayImage=1", {
    waitUntil: "domcontentloaded",
  });
  await expect(page.locator(".annotation-marker")).toHaveCount(1);
  await page.evaluate(() =>
    window.annotationTest.emit("finish-screen-annotation"),
  );
  expect(
    await page.evaluate(() => window.annotationTest.submissions.length),
  ).toBe(0);
  releaseImage();
  await expect
    .poll(() => page.evaluate(() => window.annotationTest.submissions.length))
    .toBe(1);
  const dimensions = await page.evaluate(async () => {
    const image = new Image();
    image.src = window.annotationTest.submissions[0].pngDataUrl;
    await image.decode();
    return [image.width, image.height];
  });
  expect(dimensions).toEqual([2560, 1440]);
});
