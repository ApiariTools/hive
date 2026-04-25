import { useEffect, useState } from "react";

/**
 * Returns the current iOS keyboard height in pixels.
 * Uses visualViewport API to detect when the keyboard opens
 * and calculate how much space it covers.
 * Returns 0 on non-iOS or when keyboard is closed.
 */
export function useKeyboardHeight(): number {
  const [height, setHeight] = useState(0);

  useEffect(() => {
    const vv = window.visualViewport;
    if (!vv) return;

    // Only needed on iOS
    const isIOS =
      /iPad|iPhone|iPod/.test(navigator.userAgent) ||
      (navigator.platform === "MacIntel" && navigator.maxTouchPoints > 1);
    if (!isIOS) return;

    let isKeyboardVisible = false;

    function update() {
      if (!vv) return;
      const kbHeight = window.innerHeight - vv.height;
      setHeight(isKeyboardVisible ? Math.max(0, kbHeight) : 0);
    }

    function onViewportChange() {
      if (!isKeyboardVisible) return;
      update();
    }

    function onFocusIn() {
      isKeyboardVisible = true;
      // The keyboard animates in over ~300ms. Poll a few times to catch it.
      update();
      setTimeout(update, 100);
      setTimeout(update, 200);
      setTimeout(update, 350);
      setTimeout(update, 500);
    }

    function onFocusOut() {
      isKeyboardVisible = false;
      setHeight(0);
    }

    vv.addEventListener("resize", onViewportChange, { passive: true });
    vv.addEventListener("scroll", onViewportChange, { passive: true });
    window.addEventListener("focusin", onFocusIn, { passive: true });
    window.addEventListener("focusout", onFocusOut, { passive: true });

    return () => {
      vv.removeEventListener("resize", onViewportChange);
      vv.removeEventListener("scroll", onViewportChange);
      window.removeEventListener("focusin", onFocusIn);
      window.removeEventListener("focusout", onFocusOut);
    };
  }, []);

  return height;
}
