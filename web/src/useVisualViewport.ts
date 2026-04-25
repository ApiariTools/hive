import { useEffect } from "react";

/**
 * On iOS Safari, the keyboard doesn't resize the layout viewport —
 * it overlays it and shrinks the visual viewport instead.
 * This hook listens to visualViewport resize/scroll events and
 * sets a CSS custom property so we can size the app to the
 * actual visible area, keeping the header pinned.
 */
export function useVisualViewport() {
  useEffect(() => {
    const vv = window.visualViewport;
    if (!vv) return;

    function onViewportChange() {
      const vv = window.visualViewport!;
      // Set the root element height to the visual viewport height
      // This shrinks the app when the keyboard opens
      document.documentElement.style.setProperty(
        "--app-height",
        `${vv.height}px`,
      );
      // Offset for any scroll iOS does when focusing an input
      document.documentElement.style.setProperty(
        "--app-offset",
        `${vv.offsetTop}px`,
      );
    }

    onViewportChange();
    vv.addEventListener("resize", onViewportChange);
    vv.addEventListener("scroll", onViewportChange);

    return () => {
      vv.removeEventListener("resize", onViewportChange);
      vv.removeEventListener("scroll", onViewportChange);
    };
  }, []);
}
