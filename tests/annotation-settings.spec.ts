import { expect, test } from "@playwright/test";

for (const implementation of ["tauri", "handy_keys"]) {
  test(`shows a read-only child shortcut for ${implementation}`, async ({
    page,
  }) => {
    await page.goto(
      `/annotation-settings-test?implementation=${implementation}`,
    );
    const group = page.getByRole("group", { name: "Transcribe + Annotate" });
    const parent = group.getByRole("heading", {
      name: "Transcribe + Annotate",
    });
    const child = group.getByRole("heading", { name: "Next point" });
    await expect(child).toBeVisible();
    await expect(group.locator("kbd")).toHaveText("Ctrl");
    const parentBox = (await parent.boundingBox())!;
    const childBox = (await child.boundingBox())!;
    expect(childBox.x).toBeGreaterThan(parentBox.x);
    expect(childBox.y).toBeGreaterThan(parentBox.y);
    const row = child.locator("..");
    await expect(row.locator("button, [role=button], [tabindex]")).toHaveCount(
      0,
    );
    await expect(page.getByText("Ctrl + Space", { exact: true })).toBeVisible();
  });
}

test("hides Ctrl for custom annotation shortcuts and other platforms", async ({
  page,
}) => {
  await page.goto("/annotation-settings-test?binding=ctrl%2Balt%2Bspace");
  await expect(
    page.getByRole("heading", { name: "Transcribe + Annotate" }),
  ).toBeVisible();
  await expect(page.getByRole("heading", { name: "Next point" })).toHaveCount(
    0,
  );
  await page.goto("/annotation-settings-test?os=linux");
  await expect(
    page.getByRole("heading", { name: "Transcribe Shortcut" }),
  ).toBeVisible();
  await expect(
    page.getByRole("group", { name: "Transcribe + Annotate" }),
  ).toHaveCount(0);
});

test("uses the existing Korean label", async ({ page }) => {
  await page.goto("/annotation-settings-test?language=ko");
  await expect(page.getByRole("heading", { name: "다음 항목" })).toBeVisible();
  await expect(page.locator("kbd")).toHaveText("Ctrl");
});

test.beforeEach(async ({ page }) => {
  page.on("pageerror", (error) => console.error(error));
  await page.route("**/annotation-settings-test*", async (route) => {
    const response = await route.fetch({
      url: new URL("/", route.request().url()).href,
    });
    await route.fulfill({
      response,
      body: (await response.text()).replace(
        'src="/src/main.tsx"',
        'src="/tests/fixtures/annotation-settings.tsx"',
      ),
    });
  });
});
