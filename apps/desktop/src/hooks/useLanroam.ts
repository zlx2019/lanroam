// State from the backend: the snapshot pushed on every change, the nearby
// devices polled while they are on screen, and a transient toast.

import { useCallback, useEffect, useRef, useState } from "react";
import { listen } from "@tauri-apps/api/event";
import { api } from "../api";
import { EVENTS } from "../events";
import type { NearbyDto, Snapshot } from "../types";

/** How often the nearby list refreshes while shown */
const NEARBY_POLL_MS = 2000;

/** How long a toast stays */
const TOAST_MS = 2400;

/** The latest snapshot (null until the first arrives) */
export function useSnapshot(): Snapshot | null {
  const [snapshot, setSnapshot] = useState<Snapshot | null>(null);
  useEffect(() => {
    let alive = true;
    api
      .getSnapshot()
      .then((s) => alive && setSnapshot(s))
      .catch(console.error);
    const unlisten = listen<Snapshot>(EVENTS.SNAPSHOT, (e) => setSnapshot(e.payload));
    return () => {
      alive = false;
      unlisten.then((u) => u()).catch(console.error);
    };
  }, []);
  return snapshot;
}

/** Devices on the LAN outside the group, refreshed while `active` */
export function useNearby(active: boolean): NearbyDto[] {
  const [nearby, setNearby] = useState<NearbyDto[]>([]);
  useEffect(() => {
    if (!active) return;
    let alive = true;
    const load = () =>
      api
        .listNearby()
        .then((list) => alive && setNearby(list))
        .catch(console.error);
    load();
    const timer = setInterval(load, NEARBY_POLL_MS);
    return () => {
      alive = false;
      clearInterval(timer);
    };
  }, [active]);
  return nearby;
}

/** A short message at the bottom of the window */
export function useToast(): [string | null, (message: string) => void] {
  const [toast, setToast] = useState<string | null>(null);
  const timer = useRef<number | undefined>(undefined);
  useEffect(() => () => clearTimeout(timer.current), []);
  const show = useCallback((message: string) => {
    clearTimeout(timer.current);
    setToast(message);
    timer.current = window.setTimeout(() => setToast(null), TOAST_MS);
  }, []);
  return [toast, show];
}

/** A destructive button's first click arms it; the second, within 3 s, acts */
export function useArmed(): [string | null, (id: string, act: () => void) => void] {
  const [armed, setArmed] = useState<string | null>(null);
  const timer = useRef<number | undefined>(undefined);
  useEffect(() => () => clearTimeout(timer.current), []);
  const press = useCallback(
    (id: string, act: () => void) => {
      clearTimeout(timer.current);
      if (armed === id) {
        setArmed(null);
        act();
        return;
      }
      setArmed(id);
      timer.current = window.setTimeout(() => setArmed(null), 3000);
    },
    [armed],
  );
  return [armed, press];
}
