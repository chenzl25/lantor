/** True when Lantor runs as an installed Home Screen web app, which has no browser chrome. */
export function isStandaloneDisplay() {
  return window.matchMedia?.("(display-mode: standalone)").matches
    || (navigator as Navigator & { standalone?: boolean }).standalone === true;
}
