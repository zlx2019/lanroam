// Small controls shared by the settings page and the layout's edge
// settings: choices, switches, steppers, keycaps.

import { useEffect, useRef, useState, type CSSProperties, type ReactNode } from "react";

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

/** A percentage picked on a scale, shown as a factor (1.5×) unless
 * `format` says otherwise; `onInput` follows the slider as it moves,
 * `onChange` gets the value once it is let go */
export function Slider({
  value,
  min,
  max,
  step,
  label,
  disabled = false,
  format = (v) => `${(v / 100).toFixed(1)}×`,
  onInput,
  onChange,
}: {
  value: number;
  min: number;
  max: number;
  step: number;
  label: string;
  disabled?: boolean;
  format?: (value: number) => string;
  onInput?: (value: number) => void;
  onChange: (value: number) => void;
}) {
  const [draft, setDraft] = useState(value);
  const ref = useRef<HTMLInputElement>(null);
  const changed = useRef(onChange);
  changed.current = onChange;
  useEffect(() => setDraft(value), [value]);
  // The native change event fires once the value is settled (released, or
  // stepped with the keyboard), not on every move
  useEffect(() => {
    const el = ref.current;
    if (!el) return;
    const settle = () => changed.current(Number(el.value));
    el.addEventListener("change", settle);
    return () => el.removeEventListener("change", settle);
  }, []);
  // How far the track is filled, for the stylesheet
  const fill = `${((draft - min) / (max - min)) * 100}%`;
  return (
    <span className="slider" style={{ "--fill": fill } as CSSProperties}>
      <input
        ref={ref}
        type="range"
        min={min}
        max={max}
        step={step}
        value={draft}
        disabled={disabled}
        aria-label={label}
        onChange={(e) => {
          setDraft(Number(e.target.value));
          onInput?.(Number(e.target.value));
        }}
      />
      <span className="muted">{format(draft)}</span>
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
