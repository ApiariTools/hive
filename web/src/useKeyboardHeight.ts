import { useEffect } from "react";

/**
 * Telegram's approach: set --vh CSS custom property from visualViewport.height.
 * The app root uses `height: calc(var(--vh, 1vh) * 100)` instead of `100dvh`.
 * When the keyboard opens, --vh shrinks → app shrinks → input stays visible.
 * When the keyboard closes, --vh grows → app grows back to full height.
 */
export function useVH() {
  useEffect(() => {
    const vv = window.visualViewport;

    function setVH() {
      const h = vv ? vv.height : window.innerHeight;
      const vh = h * 0.01;
      document.documentElement.style.setProperty("--vh", `${vh}px`);
    }

    setVH();

    if (vv) {
      vv.addEventListener("resize", setVH, { passive: true });
    }
    window.addEventListener("resize", setVH, { passive: true });

    return () => {
      if (vv) vv.removeEventListener("resize", setVH);
      window.removeEventListener("resize", setVH);
    };
  }, []);
}
