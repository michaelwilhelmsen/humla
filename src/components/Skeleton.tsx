type Props = {
  lines?: number;
  className?: string;
};

const WIDTHS = ["95%", "88%", "72%", "94%", "60%", "82%"];

export function SkeletonLines({ lines = 4, className = "" }: Props) {
  return (
    <div className={`flex flex-col gap-2 ${className}`}>
      {Array.from({ length: lines }).map((_, i) => (
        <div
          key={i}
          className="skeleton h-3"
          style={{ width: WIDTHS[i % WIDTHS.length], animationDelay: `${i * 120}ms` }}
        />
      ))}
    </div>
  );
}
