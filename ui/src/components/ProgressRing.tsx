import { useProgressAria } from "./ProgressBar";

interface Props {
  /** 0..1 */
  progress: number;
  /** Accessible name — what's downloading/loading (e.g. "Bringing Llama 3.1
   * 8B home…"). Required: unlike the visible "42%" text baked into the ring
   * itself, screen readers need to know *what* is 42% done. */
  label: string;
  size?: number;
  stroke?: number;
}

export default function ProgressRing({ progress, label, size = 168, stroke = 9 }: Props) {
  const radius = (size - stroke) / 2;
  const circumference = 2 * Math.PI * radius;
  const clamped = Math.min(1, Math.max(0, progress));
  // Same role="progressbar" + aria-valuenow semantics as ProgressBar (findings
  // L6/P2/D9) instead of a static, one-time role="img" aria-label snapshot
  // (finding D8) — updates as the download proceeds are now announced like
  // any other progress bar's would be, not frozen at whatever percent the
  // ring first rendered at.
  const aria = useProgressAria(progress, label);

  return (
    <svg width={size} height={size} viewBox={`0 0 ${size} ${size}`} {...aria}>
      <circle
        cx={size / 2}
        cy={size / 2}
        r={radius}
        fill="none"
        stroke="var(--line-soft)"
        strokeWidth={stroke}
        aria-hidden="true"
      />
      <circle
        cx={size / 2}
        cy={size / 2}
        r={radius}
        fill="none"
        stroke="var(--accent)"
        strokeWidth={stroke}
        strokeLinecap="round"
        strokeDasharray={circumference}
        strokeDashoffset={circumference * (1 - clamped)}
        transform={`rotate(-90 ${size / 2} ${size / 2})`}
        style={{ transition: "stroke-dashoffset 0.4s ease" }}
        aria-hidden="true"
      />
      <text
        x="50%"
        y="50%"
        dominantBaseline="central"
        textAnchor="middle"
        fill="var(--ink)"
        fontFamily="var(--font-display)"
        fontWeight={700}
        fontSize={size / 5.2}
        aria-hidden="true"
      >
        {Math.round(clamped * 100)}%
      </text>
    </svg>
  );
}
