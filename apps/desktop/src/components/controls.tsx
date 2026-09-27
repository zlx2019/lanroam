// Small controls shared by the settings page and the layout's edge
// settings: choices, switches, steppers, keycaps.

import type { ReactNode } from "react";

/** A segmented choice */
export function Seg<T extends string>({
  options,
  value,
  onChange,
}: {
  options: [T, string][];
  value: T;
  onChange: (value: T) => void;
}) {
  return (
    <span className="seg">
      {options.map(([id, label]) => (
        <button key={id} className={value === id ? "on" : ""} onClick={() => onChange(id)}>
          {label}
        </button>
      ))}
    </span>
  );
}

/** One row of a settings group */
export function Row({ title, hint, children }: { title: string; hint?: string; children: ReactNode }) {
  return (
    <div className="srow">
      <div className="t">
        <b>{title}</b>
        {hint && <small>{hint}</small>}
      </div>
      {children}
    </div>
  );
}

/** An on/off switch */
export function Toggle({ on, label, onChange }: { on: boolean; label: string; onChange: (on: boolean) => void }) {
  return <button className={`tg${on ? " on" : ""}`} onClick={() => onChange(!on)} aria-label={label} />;
}

/** A number moved in steps between `min` and `max`, shown with its unit */
export function Stepper({
  value,
  min,
  max,
  step,
  unit,
  onChange,
}: {
  value: number;
  min: number;
  max: number;
  step: number;
  unit: string;
  onChange: (value: number) => void;
}) {
  const to = (next: number) => onChange(Math.min(max, Math.max(min, next)));
  return (
    <span className="stepper">
      <button onClick={() => to(value - step)} disabled={value <= min} aria-label="−">
        −
      </button>
      <span>
        {value} {unit}
      </span>
      <button onClick={() => to(value + step)} disabled={value >= max} aria-label="+">
        +
      </button>
    </span>
  );
}

/** Keycaps of a combination */
export function Keycaps({ keys }: { keys: string[] }) {
  return (
    <>
      {keys.map((k, i) => (
        <span key={i} className="kbd">
          {k}
        </span>
      ))}
    </>
  );
}
