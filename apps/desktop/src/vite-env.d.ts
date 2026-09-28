/// <reference types="vite/client" />

interface Window {
  /** Set by the app before any script runs: the window sits on a material
   * (see material.rs) */
  __LANROAM_MATERIAL__?: boolean;
}
