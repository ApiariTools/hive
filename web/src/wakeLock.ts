let wakeLock: WakeLockSentinel | null = null;

async function requestWakeLock() {
  if (!("wakeLock" in navigator)) return;
  try {
    wakeLock = await navigator.wakeLock.request("screen");
    wakeLock.addEventListener("release", () => {
      wakeLock = null;
    });
  } catch {
    // Request can fail if page is hidden or permission denied — ignore
  }
}

function onVisibilityChange() {
  if (document.visibilityState === "visible") {
    requestWakeLock();
  }
}

export function initWakeLock() {
  requestWakeLock();
  document.addEventListener("visibilitychange", onVisibilityChange);
}
