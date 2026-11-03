// Browser-only evidence helper. IntersectionObserver clips the marker itself
// through all ancestor browsing contexts, including cross-origin frames.
async function visiblyPainted(element) {
  return element.evaluate(async (node) => {
    if (!(node instanceof HTMLElement) || !node.isConnected || document.hidden)
      return false;
    const rect = node.getBoundingClientRect();
    if (rect.width <= 0 || rect.height <= 0) return false;
    for (let ancestor = node; ancestor; ancestor = ancestor.parentElement) {
      const css = getComputedStyle(ancestor);
      if (
        css.display === "none" ||
        css.visibility === "hidden" ||
        css.visibility === "collapse" ||
        Number(css.opacity) === 0 ||
        css.contentVisibility === "hidden"
      )
        return false;
    }
    return new Promise((resolve) => {
      const observer = new IntersectionObserver(([entry]) => {
        observer.disconnect();
        resolve(
          node.isConnected &&
            !document.hidden &&
            entry.isIntersecting &&
            entry.intersectionRect.width > 0 &&
            entry.intersectionRect.height > 0,
        );
      });
      observer.observe(node);
    });
  });
}

async function marker(page, text, id) {
  for (const element of await page.locator(`#${id} iframe`).elementHandles()) {
    const root = await element.contentFrame();
    if (!root) continue;
    const frames = [root];
    for (let index = 0; index < frames.length; index++) {
      const frame = frames[index];
      frames.push(...frame.childFrames());
      try {
        let ancestorsVisible = true;
        for (
          let ancestor = frame;
          ancestor && ancestor !== page.mainFrame();
          ancestor = ancestor.parentFrame()
        ) {
          // Intersection alone does not reject transparent ancestor frames.
          if (!(await visiblyPainted(await ancestor.frameElement()))) {
            ancestorsVisible = false;
            break;
          }
        }
        if (!ancestorsVisible) continue;
        for (const textElement of await frame
          .getByText(text, { exact: false })
          .elementHandles()) {
          if (await visiblyPainted(textElement)) return true;
        }
      } catch {
        // Detached, hidden, collapsed and offscreen frames are not evidence.
      }
    }
  }
  return false;
}

module.exports = { visiblyPainted, marker };
