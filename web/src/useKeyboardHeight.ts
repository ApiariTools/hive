import { useEffect, useState } from "react";

export function useKeyboardHeight(): number {
  const [height, setHeight] = useState(0);

  useEffect(() => {
    const vv = window.visualViewport;
    if (!vv) return;

    const isIOS =
      /iPad|iPhone|iPod/.test(navigator.userAgent) ||
      (navigator.platform === "MacIntel" && navigator.maxTouchPoints > 1);
    if (!isIOS) return;

    function update() {
      if (!vv) return;
      const kbHeight = window.innerHeight - vv.height;
      // If keyboard height is very small, treat as closed
      setHeight(kbHeight > 50 ? kbHeight : 0);
    }

    function onViewportChange() {
      update();
    }

    function onFocusIn() {
      update();
      setTimeout(update, 100);
      setTimeout(update, 200);
      setTimeout(update, 350);
      setTimeout(update, 500);
    }

    function onFocusOut() {
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
