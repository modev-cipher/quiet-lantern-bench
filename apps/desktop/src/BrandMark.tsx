function BrandMark({ title }: { title?: string }) {
  return (
    <span
      data-testid="brand-mark"
      title={title}
      className="flex h-8 w-8 shrink-0 items-center justify-center"
    >
      <svg
        viewBox="0 6 82 90"
        role="img"
        aria-label="txtCaster"
        className="h-7 w-7"
      >
        <path
          className="fill-ink-100"
          d="M22 18 L34 6 L34 62 L45 73 L36.5 81.5 L22 67 Z"
        />
        <path className="fill-ink-100" d="M8 30 L74 30 L62 42 L8 42 Z" />
        <path
          className="fill-[#7ba3ff]"
          d="M76 52 L76 58 L70 64 L54 64 L54 78 L76 78 L76 90 L42 90 L42 52 Z"
        />
      </svg>
    </span>
  );
}

export default BrandMark;
