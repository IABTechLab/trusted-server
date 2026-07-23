import { expect, test } from "@playwright/test";
import { runtimeUrl } from "../../helpers/state.js";

test("console query alone does not install ad trace when config is disabled", async ({
    page,
}) => {
    await page.goto(runtimeUrl("/?ts_console=1"), {
        waitUntil: "domcontentloaded",
    });

    await expect
        .poll(() =>
            page.evaluate(
                () =>
                    typeof (window as Window & { tsjs?: { adTrace?: unknown } })
                        .tsjs?.adTrace,
            ),
        )
        .toBe("undefined");
});
